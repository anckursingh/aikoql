"""T-16: the POC chatbot — chat() wired to the real model and a live
AikoQL server (design §40 end-state).

Run: PYTHONPATH=training/src python training/scripts/chat.py \
       --db 127.0.0.1:61146 --token acme [--adapter dir] [--question "..."]
No --question: an interactive loop, one line per question, EOF/quit to
exit. The model is the T-15 LoRA base (adapter optional); the query
runner is the live aikoql() call exactly as finetune.py predict.

Ponytail ceilings: the model-load/generate preamble duplicates
finetune.py predict (~20 lines) — scripts are standalone entry points
by repo convention; greedy decoding keeps replies reproducible.
"""

from __future__ import annotations

import argparse
import sys

from aikoql_training.chat import chat

# ponytail: duplicated from finetune.py — scripts are standalone
# entry points by repo convention (no script-to-script imports)
MODEL_ID = "Qwen/Qwen2.5-0.5B-Instruct"


def _resolve_device(device: str) -> str:
    if device != "auto":
        return device
    import torch
    return "cuda" if torch.cuda.is_available() else "cpu"


def _generate(model, tokenizer, max_new_tokens: int = 64):
    import torch

    def generate(text: str) -> str:
        enc = tokenizer([text], return_tensors="pt")
        pl = enc["attention_mask"].sum(dim=1)
        with torch.no_grad():
            gen = model.generate(**enc, max_new_tokens=max_new_tokens,
                                 do_sample=False,
                                 pad_token_id=tokenizer.pad_token_id)
        return tokenizer.batch_decode(
            [gen[0, pl[0]:].tolist()], skip_special_tokens=True)[0]

    return generate


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--db", required=True, help="server host:port")
    parser.add_argument("--token", required=True, help="client token")
    parser.add_argument("--adapter", default=None,
                        help="LoRA adapter directory (finetuned model)")
    parser.add_argument("--device", default="auto",
                        choices=("auto", "cuda", "cpu"))
    parser.add_argument("--question", default=None,
                        help="one-shot question (default: interactive)")
    args = parser.parse_args(argv)

    import torch
    from transformers import AutoModelForCausalLM, AutoTokenizer

    tokenizer = AutoTokenizer.from_pretrained(MODEL_ID)
    tokenizer.pad_token = tokenizer.eos_token
    tokenizer.padding_side = "left"
    model = AutoModelForCausalLM.from_pretrained(MODEL_ID)
    if args.adapter:
        from peft import PeftModel
        model = PeftModel.from_pretrained(model, args.adapter)
    model = model.to(_resolve_device(args.device))
    model.eval()

    import aikoql
    generate = _generate(model, tokenizer)
    # the corpus server right after seeding: tantivy churn makes the
    # default socket timeout flake (gates.py, T-14) — connect wide
    with aikoql.Agent.connect(args.db, token=args.token,
                              timeout=60.0) as agent:

        def run_query(query):
            return agent.aikoql(query)

        def one(question: str) -> None:
            rec = chat(question, generate=generate, run_query=run_query)
            print(f"query: {rec['query'] or '-'}")
            print(f"compiled={rec['compiled']} retrieved={len(rec['retrieved'])}")
            print(f"answer: {rec['answer']}")

        if args.question:
            one(args.question)
            return 0
        print("chat (empty line or EOF to exit)")
        for line in sys.stdin:
            question = line.strip()
            if not question:
                break
            one(question)
    return 0


if __name__ == "__main__":
    sys.exit(main())
