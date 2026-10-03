"""T-15: LoRA fine-tune + prediction for the 0.5B-class POC model
(design Phase 18/19, §32/33/34).

`train` instruction-tunes Qwen2.5-0.5B-Instruct on the corpus split
with two skills per example — question -> AikoQL query (QUERY: marker)
and question + context -> grounded answer / UNKNOWN: refusal — LoRA
over all linear layers (peft), completion-only labels. The design law
(plan §3, line 64) is enforced: training refuses to start until a
scorecard artifact exists (run a baseline first — no training run
without one).

`predict` runs the two skills over a split (batched, greedy) and
writes predictions.jsonl for the scorecard; with --db/--token each
query is compiled and executed live and its retrieved koids recorded,
so ko_recall/ko_precision measure the real pipeline.

Ponytail ceilings: fp32 throughout (no bf16 autoconversion — a laptop
POC, and the 1650's 4 GB holds the 0.5B base in fp32); --device
defaults to cuda when torch sees a GPU, else cpu; the
prompt/completion token split assumes the prefix property (a BPE
boundary merge mislabels one token, never more); greedy decoding
keeps predictions reproducible.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any, Dict, List, Tuple

from aikoql_training.dataset.writer import read_dataset
from aikoql_training.inference import (
    build_answer_prompt,
    build_query_prompt,
    parse_model_reply,
)

MODEL_ID = "Qwen/Qwen2.5-0.5B-Instruct"  # the RED pin: a 0.5B-class open model
_SCORECARDS = Path(__file__).parents[1] / "artifacts" / "scorecards"


def _require_scorecard() -> None:
    """Design law (plan §3): no training run without a scorecard."""
    if not list(_SCORECARDS.glob("*.json")):
        sys.exit("no scorecard artifact found under training/artifacts/"
                 "scorecards — run a baseline scorecard first "
                 "(design: no training run without one)")


def _statements(ex: Dict[str, Any]) -> List[str]:
    return [f.get("statement") for f in
            (ex.get("context") or {}).get("facts") or []
            if isinstance(f.get("statement"), str)]


def _rows(examples: List[dict], max_rows: int) -> List[Tuple[str, str]]:
    """Two skill rows per example: (prompt, completion)."""
    rows: List[Tuple[str, str]] = []
    for ex in examples:
        question = str((ex.get("input") or {}).get("question") or "")
        query = str((ex.get("query_target") or {}).get("query") or "")
        answer = str((ex.get("expected") or {}).get("answer") or "")
        rows.append((build_query_prompt(question), f"QUERY: {query}\n"))
        rows.append((build_answer_prompt(question, _statements(ex)),
                     f"{answer}\n"))
    return rows[:max_rows] if max_rows else rows


def _load_tokenizer():
    from transformers import AutoTokenizer
    tok = AutoTokenizer.from_pretrained(MODEL_ID)
    tok.pad_token = tok.eos_token  # Qwen2 has no pad token
    return tok


def _encode_rows(tokenizer, rows: List[Tuple[str, str]], max_len: int):
    encs = []
    for prompt, completion in rows:
        full = tokenizer(prompt + completion, truncation=True,
                         max_length=max_len)["input_ids"]
        n_prompt = len(tokenizer(prompt, truncation=True,
                                 max_length=max_len)["input_ids"])
        # ponytail: a BPE merge across the boundary mislabels one token
        labels = [-100] * n_prompt + full[n_prompt:]
        encs.append({"input_ids": full, "labels": labels,
                     "attention_mask": [1] * len(full)})
    return encs


def _collate(batch: List[dict], pad_id: int) -> Dict[str, Any]:
    import torch
    max_len = max(len(b["input_ids"]) for b in batch)
    out: Dict[str, List[List[int]]] = {"input_ids": [], "attention_mask": [],
                                       "labels": []}
    for b in batch:
        n = max_len - len(b["input_ids"])
        out["input_ids"].append(b["input_ids"] + [pad_id] * n)
        out["attention_mask"].append(b["attention_mask"] + [0] * n)
        out["labels"].append(b["labels"] + [-100] * n)
    return {k: torch.tensor(v) for k, v in out.items()}


def _resolve_device(device: str) -> str:
    """auto -> cuda when torch sees a GPU, else cpu (fp32 fits 4 GB)."""
    if device != "auto":
        return device
    import torch
    return "cuda" if torch.cuda.is_available() else "cpu"


def train(args) -> int:
    _require_scorecard()
    import torch
    from peft import LoraConfig, get_peft_model
    from transformers import AutoModelForCausalLM, Trainer, TrainingArguments

    ds = read_dataset(args.dataset)
    rows = _rows(ds[args.split], args.max_rows)
    if not rows:
        sys.exit(f"split {args.split} carries no examples")
    tokenizer = _load_tokenizer()
    tokenizer.padding_side = "right"
    encs = _encode_rows(tokenizer, rows, args.seq_len)

    class Rows(torch.utils.data.Dataset):
        def __len__(self):
            return len(encs)

        def __getitem__(self, i):
            return encs[i]

    model = AutoModelForCausalLM.from_pretrained(MODEL_ID)
    model = get_peft_model(model, LoraConfig(
        r=args.lora_r, lora_alpha=args.lora_alpha,
        target_modules="all-linear", lora_dropout=0.05, bias="none",
        task_type="CAUSAL_LM"))
    model.print_trainable_parameters()
    trainer = Trainer(
        model=model,
        args=TrainingArguments(
            output_dir=str(args.out),
            per_device_train_batch_size=args.batch_size,
            gradient_accumulation_steps=2,
            num_train_epochs=args.epochs,
            learning_rate=2e-4,
            logging_steps=5,
            save_strategy="no",
            report_to=[],
            seed=0,
            use_cpu=(_resolve_device(args.device) == "cpu"),
        ),
        train_dataset=Rows(),
        data_collator=lambda batch: _collate(batch, tokenizer.pad_token_id),
    )
    print(f"rows={len(rows)} split={args.split} model={MODEL_ID}")
    trainer.train()
    model.save_pretrained(args.out)  # the LoRA adapter
    tokenizer.save_pretrained(args.out)
    print(f"adapter saved to {args.out}")
    return 0


def _generate(model, tokenizer, prompts: List[str],
              batch_size: int, max_new_tokens: int) -> List[str]:
    import torch
    tokenizer.padding_side = "left"
    texts: List[str] = []
    for i in range(0, len(prompts), batch_size):
        chunk = prompts[i:i + batch_size]
        enc = tokenizer(chunk, padding=True, truncation=True,
                        max_length=256, return_tensors="pt")
        prompt_lens = enc["attention_mask"].sum(dim=1)
        with torch.no_grad():
            gen = model.generate(
                **enc, max_new_tokens=max_new_tokens, do_sample=False,
                pad_token_id=tokenizer.pad_token_id)
        texts += tokenizer.batch_decode(
            [gen[j, pl:].tolist() for j, pl in enumerate(prompt_lens)],
            skip_special_tokens=True)
    return texts


def predict(args) -> int:
    import torch
    from transformers import AutoModelForCausalLM

    ds = read_dataset(args.dataset)
    examples = ds[args.split]
    if args.max_examples:
        examples = examples[:args.max_examples]
    tokenizer = _load_tokenizer()
    model = AutoModelForCausalLM.from_pretrained(MODEL_ID)
    if args.adapter:
        from peft import PeftModel
        model = PeftModel.from_pretrained(model, args.adapter)
    model = model.to(_resolve_device(args.device))
    model.eval()

    questions = [str((e.get("input") or {}).get("question") or "")
                 for e in examples]
    query_replies = _generate(model, tokenizer,
                              [build_query_prompt(q) for q in questions],
                              args.batch_size, args.max_new_tokens)
    answer_replies = _generate(
        model, tokenizer,
        [build_answer_prompt(q, _statements(e))
         for q, e in zip(questions, examples)],
        args.batch_size, args.max_new_tokens)

    live = args.db is not None
    predictions: List[dict] = []
    for e, q_reply, a_reply in zip(examples, query_replies, answer_replies):
        query, _ = parse_model_reply(q_reply)
        _, answer = parse_model_reply(a_reply)
        if not answer:
            answer = a_reply.strip()
        pred: Dict[str, Any] = {"example_id": e["example_id"],
                                "query": query, "answer": answer}
        predictions.append(pred)

    if live:
        import aikoql
        # the corpus server right after seeding: tantivy churn makes the
        # default socket timeout flake (gates.py, T-14) — connect wide
        with aikoql.Agent.connect(args.db, token=args.token,
                                  timeout=60.0) as agent:
            for pred in predictions:
                if not pred["query"]:
                    pred["compiled"] = False
                    pred["retrieved"] = []
                    continue
                try:
                    env = agent.aikoql(pred["query"])
                    pred["compiled"] = True
                    pred["retrieved"] = [r.get("koid") for r in
                                         env.get("results") or []
                                         if r.get("koid")]
                except Exception:
                    pred["compiled"] = False
                    pred["retrieved"] = []

    with open(args.out, "w", encoding="utf-8", newline="\n") as f:
        for pred in predictions:
            f.write(json.dumps(pred, sort_keys=True) + "\n")
    print(json.dumps({"predictions": len(predictions), "split": args.split,
                      "live": live, "model": MODEL_ID,
                      "adapter": args.adapter}, sort_keys=True))
    return 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="mode", required=True)
    t = sub.add_parser("train")
    t.add_argument("--dataset", required=True, help="dataset directory")
    t.add_argument("--split", default="train")
    t.add_argument("--out", required=True, help="adapter output directory")
    t.add_argument("--max-rows", type=int, default=0,
                   help="cap training rows (0 = all)")
    t.add_argument("--seq-len", type=int, default=256)
    t.add_argument("--batch-size", type=int, default=2)
    t.add_argument("--epochs", type=float, default=1.0)
    t.add_argument("--lora-r", type=int, default=4)
    t.add_argument("--lora-alpha", type=int, default=8)
    t.add_argument("--device", default="auto",
                   choices=("auto", "cuda", "cpu"))
    p = sub.add_parser("predict")
    p.add_argument("--dataset", required=True, help="dataset directory")
    p.add_argument("--split", default="test")
    p.add_argument("--out", required=True, help="predictions.jsonl path")
    p.add_argument("--adapter", default=None, help="LoRA adapter directory")
    p.add_argument("--db", default=None, help="server host:port (live run)")
    p.add_argument("--token", default=None, help="client token (with --db)")
    p.add_argument("--batch-size", type=int, default=8)
    p.add_argument("--max-new-tokens", type=int, default=64)
    p.add_argument("--max-examples", type=int, default=0,
                   help="cap examples (0 = all)")
    p.add_argument("--device", default="auto",
                   choices=("auto", "cuda", "cpu"))
    args = parser.parse_args(argv)
    if args.mode == "train":
        return train(args)
    return predict(args)


if __name__ == "__main__":
    sys.exit(main())
