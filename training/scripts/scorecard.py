"""T-15: write a model scorecard artifact (design §32).

Loads a dataset split, joins the prediction records produced by
finetune.py predict, computes the six metrics and writes the
scorecard JSON under training/artifacts/scorecards/. The artifact is
the committed evidence a training run may point at (the design law:
the scorecard precedes any training run).
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import subprocess
import sys
from pathlib import Path

from aikoql_training.dataset.writer import read_dataset
from aikoql_training.scorecard import compute_scorecard

_ARTIFACTS = Path(__file__).parents[1] / "artifacts" / "scorecards"


def _revision() -> str:
    try:
        out = subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True,
                             text=True, check=True)
        return out.stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return ""


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--dataset", required=True, help="dataset directory")
    parser.add_argument("--predictions", required=True,
                        help="predictions.jsonl from finetune.py predict")
    parser.add_argument("--model-id", required=True, help="the HF model id")
    parser.add_argument("--model-class", required=True,
                        choices=("baseline", "finetuned"))
    parser.add_argument("--adapter", default=None,
                        help="adapter directory (finetuned runs)")
    parser.add_argument("--split", default="test")
    parser.add_argument("--out", default=None,
                        help="output path (default: "
                             "training/artifacts/scorecards/)")
    args = parser.parse_args(argv)

    ds = read_dataset(args.dataset)
    with open(args.predictions, encoding="utf-8") as f:
        predictions = [json.loads(line) for line in f if line.strip()]
    score = compute_scorecard(predictions, ds, split=args.split)
    manifest = ds["manifest"]
    artifact = {
        "model_id": args.model_id,
        "model_class": args.model_class,
        "adapter": args.adapter,
        "dataset_id": manifest["dataset_id"],
        "seed": manifest["seed"],
        "split": args.split,
        "revision": _revision(),
        "created_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        **score,
    }
    out = Path(args.out) if args.out else (
        _ARTIFACTS / f"scorecard-{args.model_class}.json")
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(artifact, sort_keys=True, indent=2) + "\n",
                   encoding="utf-8")
    print(json.dumps({"path": str(out), "example_count":
                      artifact["example_count"],
                      "metrics": {k: v["value"] for k, v in
                                  artifact["metrics"].items()}},
                     sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
