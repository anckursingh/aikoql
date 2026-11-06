#!/usr/bin/env python3
"""Optional: score Docling on the extraction-bench corpus next to AikoQL.

Docling is never a dependency of AikoQL; this script only compares. It
skips cleanly (exit 0) when `docling` is not importable:

    python3.12 -m venv /tmp/dl && /tmp/dl/bin/pip install docling
    /tmp/dl/bin/python scripts/bench-vs-docling.py

AikoQL's numbers come from the Rust benchmark (`cargo test -p
aikoql-ingestion --test extraction_quality_bench -- --nocapture`, the
`[XBENCH-JSON]` line); pass --aikoql-json FILE to skip running cargo. The
scorers below mirror crates/ingestion/tests/extraction_quality_bench.rs.
Docling does no typed entity extraction, so entity cells print N/A.
First Docling run downloads its layout/table models (network).
"""
import json
import os
import re
import subprocess
import sys

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
DIR = os.path.join(ROOT, "crates", "ingestion", "tests", "fixtures", "extraction-bench")
FIXTURES = [
    "invoice.pdf",
    "entities.txt",
    "scanned-table.png",
    "scanned-table-skewed.png",
    "scanned-letter.png",
]


def norm(s):
    return " ".join(s.split()).rstrip(".,;:").lower()


def lev(a, b):
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a):
        cur = [i + 1] + [0] * len(b)
        for j, cb in enumerate(b):
            cur[j + 1] = min(prev[j] + (ca != cb), prev[j + 1] + 1, cur[j] + 1)
        prev = cur
    return prev[len(b)]


def ocr_accuracy(pred, gold):
    clean = lambda s: " ".join(s.replace("|", " ").split())
    p, g = clean(pred), clean(gold)
    return max(0.0, 1 - lev(p, g) / len(g)) if g else 1.0


def cell_accuracy(pred, gold):
    hit = sum(
        1
        for r, row in enumerate(gold)
        for c, cell in enumerate(row)
        if r < len(pred) and c < len(pred[r]) and norm(str(pred[r][c])) == norm(cell)
    )
    count = lambda g: sum(len(r) for r in g)
    return hit / max(count(gold), count(pred), 1)


def mean(v):
    return sum(v) / len(v) if v else None


def docling_metrics():
    from docling.document_converter import DocumentConverter

    conv = DocumentConverter()
    pdf_cells, scan_cells, ocr, fps = [], [], [], 0
    for name in FIXTURES:
        gt = json.load(open(os.path.join(DIR, name + ".gt.json")))
        if gt["mime"] == "text/plain":
            continue  # Docling's text path is markdown/asciidoc; entities are N/A anyway
        doc = conv.convert(os.path.join(DIR, name)).document
        by_page = {}
        for t in doc.tables:
            page = t.prov[0].page_no if t.prov else 1
            df = t.export_to_dataframe(doc=doc)
            body = [list(map(str, r)) for r in df.values.tolist()]
            # No detected header: pandas numbers the columns 0..n-1 — the
            # first body row is then the header, not the column index.
            numbered = list(df.columns) == list(range(len(df.columns)))
            grid = body if numbered else [list(map(str, df.columns))] + body
            by_page.setdefault(page, []).append(grid)
        for t in gt["tables"]:
            best = max((cell_accuracy(g, t["rows"]) for g in by_page.get(t["page"], [])), default=0.0)
            print(f"[docling {name}] table p{t['page']}: cell accuracy {best:.3f}")
            (scan_cells if gt["mime"].startswith("image/") else pdf_cells).append(best)
        for p in gt["no_table_pages"]:
            fps += len(by_page.get(p, []))
        if "ocr_text" in gt:
            acc = ocr_accuracy(doc.export_to_text(), gt["ocr_text"])
            print(f"[docling {name}] ocr char accuracy {acc:.3f}")
            ocr.append(acc)
    return {
        "table_cell_accuracy_pdf": mean(pdf_cells),
        "table_cell_accuracy_scan": mean(scan_cells),
        "ocr_char_accuracy": mean(ocr),
        "table_false_positives": float(fps),
    }


def aikoql_metrics(argv):
    if "--aikoql-json" in argv:
        return json.load(open(argv[argv.index("--aikoql-json") + 1]))
    out = subprocess.run(
        ["cargo", "test", "-q", "-p", "aikoql-ingestion", "--test", "extraction_quality_bench",
         "--", "--nocapture", "extraction_quality_ratchet"],
        cwd=ROOT, capture_output=True, text=True,
    )
    m = re.search(r"\[XBENCH-JSON\] (\{.*\})", out.stdout + out.stderr)
    if not m:
        print("could not run the AikoQL benchmark:\n" + out.stderr[-2000:])
        return {}
    return json.loads(m.group(1))


def main():
    try:
        import docling  # noqa: F401
    except ImportError:
        print("docling not importable — skipping the comparison (pip install docling)")
        return 0
    from importlib.metadata import version

    dl = docling_metrics()
    ak = aikoql_metrics(sys.argv)
    fmt = lambda v: "N/A" if v is None else f"{v:.3f}"
    print(f"\n{'metric':28} {'aikoql':>8} {'docling ' + version('docling'):>16}")
    for k in ["entity_precision", "entity_recall", "table_cell_accuracy_pdf",
              "table_cell_accuracy_scan", "table_false_positives", "ocr_char_accuracy"]:
        print(f"{k:28} {fmt(ak.get(k)):>8} {fmt(dl.get(k)):>16}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
