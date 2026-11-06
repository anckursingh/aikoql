#!/usr/bin/env python3
"""Generate the extraction-quality benchmark corpus (M1).

Writes crates/ingestion/tests/fixtures/extraction-bench/*. The outputs are
committed, so tests never need this script or its venv. Native PDFs are
hand-written PDF syntax (no dependency); the raster images, XLSX and PPTX
need Pillow / openpyxl / python-pptx:

    python3 -m venv /tmp/fx && /tmp/fx/bin/pip install pillow openpyxl python-pptx
    /tmp/fx/bin/python scripts/gen-extraction-bench-fixtures.py

Ground truth lives next to each fixture as <name>.gt.json and is written by
hand below — never derived from an extractor's output.
"""
import json
import os
import sys

OUT = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    "..",
    "crates",
    "ingestion",
    "tests",
    "fixtures",
    "extraction-bench",
)
FONT = "/System/Library/Fonts/Supplemental/Arial.ttf"


# ---------------------------------------------------------------- PDF writer
def pdf_escape(s):
    return s.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")


def text_op(x, y, s, size=10):
    return f"BT /F1 {size} Tf {x} {y} Td ({pdf_escape(s)}) Tj ET"


def write_pdf(path, pages):
    """pages: list of content-stream strings (612x792 pt, Helvetica)."""
    objs = []
    objs.append("<< /Type /Catalog /Pages 2 0 R >>")
    kids = " ".join(f"{4 + 2 * i} 0 R" for i in range(len(pages)))
    objs.append(f"<< /Type /Pages /Kids [{kids}] /Count {len(pages)} >>")
    objs.append(
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
    )
    for i, content in enumerate(pages):
        content_id = 5 + 2 * i
        objs.append(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            f"/Resources << /Font << /F1 3 0 R >> >> /Contents {content_id} 0 R >>"
        )
        data = content.encode("latin-1")
        objs.append(
            f"<< /Length {len(data)} >>\nstream\n" + content + "\nendstream"
        )
    out = bytearray(b"%PDF-1.4\n")
    offsets = []
    for n, body in enumerate(objs, start=1):
        offsets.append(len(out))
        out += f"{n} 0 obj\n{body}\nendobj\n".encode("latin-1")
    xref = len(out)
    out += f"xref\n0 {len(objs) + 1}\n0000000000 65535 f \n".encode()
    for off in offsets:
        out += f"{off:010d} 00000 n \n".encode()
    out += (
        f"trailer\n<< /Size {len(objs) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
    ).encode()
    with open(path, "wb") as f:
        f.write(out)


def ruled_table(x_cols, right, y_top, row_h, rows, size=10):
    """A table with cell borders: x_cols = left edge of each column."""
    ops = ["0.5 w"]
    n = len(rows)
    for r in range(n + 1):
        y = y_top - r * row_h
        ops.append(f"{x_cols[0]} {y} m {right} {y} l S")
    for x in x_cols + [right]:
        ops.append(f"{x} {y_top} m {x} {y_top - n * row_h} l S")
    for r, row in enumerate(rows):
        y = y_top - (r + 1) * row_h + 6
        for c, cell in enumerate(row):
            ops.append(text_op(x_cols[c] + 4, y, cell, size))
    return ops


# ------------------------------------------------------------------ invoice
INVOICE_TABLE = [
    ["Description", "Qty", "Unit price", "Amount"],
    ["Consulting services", "10", "EUR 150.00", "EUR 1,500.00"],
    ["Software licence", "2", "EUR 499.00", "EUR 998.00"],
    ["Travel expenses", "1", "EUR 320.50", "EUR 320.50"],
    ["Support plan", "12", "EUR 45.00", "EUR 540.00"],
]

INVOICE_P1_LINES = [
    (740, "INVOICE", 20),
    (712, "Northwind Trading B.V., Amsterdam, Netherlands", 10),
    (698, "Invoice number: INV-2026-0042", 10),
    (684, "Invoice date: 2026-03-14", 10),
    (660, "Bill to: Contoso Ltd, London, United Kingdom", 10),
    (646, "Contact: Dr. Emma Clarke, emma.clarke@contoso.example, +44 20 7946 0958", 10),
]
INVOICE_P1_TAIL = [
    (430, "Subtotal: EUR 3,358.50"),
    (416, "VAT (21%): EUR 705.29"),
    (402, "Total due: EUR 4,063.79"),
    (380, "Please pay to IBAN NL91 ABNA 0417 1643 00 by April 13, 2026."),
]

COL_LEFT = [
    "Northwind Trading was founded in",
    "1998 as a small importer of tea and",
    "coffee. Today the company supplies",
    "independent shops across the",
    "region from its warehouse near the",
    "port. Every order is packed by hand",
    "and shipped within two working days.",
]
COL_RIGHT = [
    "Our support team answers questions",
    "about deliveries, returns and",
    "invoices on weekdays. Customers who",
    "order every month receive a",
    "discount on shipping and early",
    "access to seasonal blends, which are",
    "announced in our printed newsletter.",
]


def invoice_pdf():
    p1 = [text_op(72, y, s, size) for (y, s, size) in INVOICE_P1_LINES]
    p1 += ruled_table([72, 290, 340, 440], 540, 610, 20, INVOICE_TABLE)
    p1 += [text_op(72, y, s) for (y, s) in INVOICE_P1_TAIL]
    p2 = [text_op(72, 740, "About us", 14)]
    for i, (l, r) in enumerate(zip(COL_LEFT, COL_RIGHT)):
        y = 712 - i * 14
        p2.append(text_op(72, y, l))
        p2.append(text_op(320, y, r))
    write_pdf(os.path.join(OUT, "invoice.pdf"), ["\n".join(p1), "\n".join(p2)])
    gt = {
        "mime": "application/pdf",
        "entities": [
            {"text": "Northwind Trading B.V.", "type": "Organization"},
            {"text": "Amsterdam", "type": "Location"},
            {"text": "Netherlands", "type": "Location"},
            {"text": "2026-03-14", "type": "Date"},
            {"text": "Contoso Ltd", "type": "Organization"},
            {"text": "London", "type": "Location"},
            {"text": "United Kingdom", "type": "Location"},
            {"text": "Emma Clarke", "type": "Person"},
            {"text": "emma.clarke@contoso.example", "type": "Email"},
            {"text": "+44 20 7946 0958", "type": "Phone"},
            {"text": "EUR 150.00", "type": "Money"},
            {"text": "EUR 1,500.00", "type": "Money"},
            {"text": "EUR 499.00", "type": "Money"},
            {"text": "EUR 998.00", "type": "Money"},
            {"text": "EUR 320.50", "type": "Money"},
            {"text": "EUR 45.00", "type": "Money"},
            {"text": "EUR 540.00", "type": "Money"},
            {"text": "EUR 3,358.50", "type": "Money"},
            {"text": "EUR 705.29", "type": "Money"},
            {"text": "EUR 4,063.79", "type": "Money"},
            {"text": "21%", "type": "Percentage"},
            {"text": "NL91 ABNA 0417 1643 00", "type": "Iban"},
            {"text": "April 13, 2026", "type": "Date"},
            {"text": "Northwind Trading", "type": "Organization"},
        ],
        "tables": [{"page": 1, "rows": INVOICE_TABLE}],
        # Page 2 is two columns of prose: any table found there is a false positive.
        "no_table_pages": [2],
    }
    with open(os.path.join(OUT, "invoice.pdf.gt.json"), "w") as f:
        json.dump(gt, f, indent=2)
        f.write("\n")


# ----------------------------------------------------------------- entities
ENTITY_TEXT = """Dr. Maria Jansen joined Utrecht International Center in March 2024 as head of research. She previously worked at Siemens AG in Munich, Germany, and at Acme Holdings Inc. in Boston.

The partnership with Globex GmbH was signed on 12 May 2025 in Berlin. The contract is worth $2.5 million, with an advance of €120,000 paid on 2025-06-01. Revenue grew 12.5% year over year.

Contact Prof. John Okafor at john.okafor@uic.example or +31 30 253 7000. More information is available at https://www.uic.example/partners.

Mr. Peter van Dijk, finance director of Rabobank, approved the transfer to IBAN DE89 3704 0044 0532 0130 00 on Friday.

The next review meeting is planned in Rotterdam on 15/09/2025 with Sarah Connor from Cyberdyne Systems Ltd.
"""


def entities_txt():
    with open(os.path.join(OUT, "entities.txt"), "w") as f:
        f.write(ENTITY_TEXT)
    gt = {
        "mime": "text/plain",
        "entities": [
            {"text": "Maria Jansen", "type": "Person"},
            {"text": "Utrecht International Center", "type": "Organization"},
            {"text": "March 2024", "type": "Date"},
            {"text": "Siemens AG", "type": "Organization"},
            {"text": "Munich", "type": "Location"},
            {"text": "Germany", "type": "Location"},
            {"text": "Acme Holdings Inc.", "type": "Organization"},
            {"text": "Boston", "type": "Location"},
            {"text": "Globex GmbH", "type": "Organization"},
            {"text": "12 May 2025", "type": "Date"},
            {"text": "Berlin", "type": "Location"},
            {"text": "$2.5 million", "type": "Money"},
            {"text": "€120,000", "type": "Money"},
            {"text": "2025-06-01", "type": "Date"},
            {"text": "12.5%", "type": "Percentage"},
            {"text": "John Okafor", "type": "Person"},
            {"text": "john.okafor@uic.example", "type": "Email"},
            {"text": "+31 30 253 7000", "type": "Phone"},
            {"text": "https://www.uic.example/partners", "type": "Url"},
            {"text": "Peter van Dijk", "type": "Person"},
            {"text": "Rabobank", "type": "Organization"},
            {"text": "DE89 3704 0044 0532 0130 00", "type": "Iban"},
            {"text": "Rotterdam", "type": "Location"},
            {"text": "15/09/2025", "type": "Date"},
            {"text": "Sarah Connor", "type": "Person"},
            {"text": "Cyberdyne Systems Ltd", "type": "Organization"},
        ],
        "tables": [],
        "no_table_pages": [1],
    }
    with open(os.path.join(OUT, "entities.txt.gt.json"), "w") as f:
        json.dump(gt, f, indent=2, ensure_ascii=False)
        f.write("\n")


# ------------------------------------------------------------ raster images
SCAN_TABLE = [
    ["Region", "Q1", "Q2", "Q3"],
    ["North America", "1,200", "1,350", "1,410"],
    ["Europe", "980", "1,020", "1,105"],
    ["Asia Pacific", "760", "840", "905"],
    ["Latin America", "310", "335", "360"],
]
SCAN_LETTER = [
    "The warehouse in Rotterdam will close for inventory",
    "on the first Monday of every quarter. Orders placed",
    "during the closure are shipped the following day.",
    "Please contact the support desk with any questions",
    "about delivery times, returns or damaged parcels.",
]


def raster_images():
    from PIL import Image, ImageDraw, ImageFont

    font = ImageFont.truetype(FONT, 34)
    title_font = ImageFont.truetype(FONT, 40)

    # Scanned table: grid lines, left-aligned cells, a title above.
    cols = [80, 520, 820, 1120]
    right, top, row_h = 1420, 180, 70
    img = Image.new("L", (1500, top + row_h * len(SCAN_TABLE) + 120), 255)
    d = ImageDraw.Draw(img)
    d.text((80, 70), "Quarterly sales by region", font=title_font, fill=0)
    n = len(SCAN_TABLE)
    for r in range(n + 1):
        y = top + r * row_h
        d.line([(cols[0], y), (right, y)], fill=0, width=2)
    for x in cols + [right]:
        d.line([(x, top), (x, top + n * row_h)], fill=0, width=2)
    for r, row in enumerate(SCAN_TABLE):
        for c, cell in enumerate(row):
            d.text((cols[c] + 16, top + r * row_h + 16), cell, font=font, fill=0)
    img.save(os.path.join(OUT, "scanned-table.png"))
    # A photocopy-like variant: 1.2 degree skew, speckle noise, soft blur.
    import random

    from PIL import ImageFilter

    rng = random.Random(7)
    skewed = img.rotate(1.2, expand=True, fillcolor=255, resample=Image.BICUBIC)
    px = skewed.load()
    w, h = skewed.size
    for _ in range(w * h // 150):
        px[rng.randrange(w), rng.randrange(h)] = rng.choice((0, 90, 160))
    skewed = skewed.filter(ImageFilter.GaussianBlur(0.8))
    skewed.save(os.path.join(OUT, "scanned-table-skewed.png"))
    ocr = "Quarterly sales by region\n" + "\n".join(" ".join(r) for r in SCAN_TABLE)
    for name in ("scanned-table.png", "scanned-table-skewed.png"):
        with open(os.path.join(OUT, name + ".gt.json"), "w") as f:
            json.dump(
                {
                    "mime": "image/png",
                    "entities": [],
                    "tables": [{"page": 1, "rows": SCAN_TABLE}],
                    "no_table_pages": [],
                    "ocr_text": ocr,
                },
                f,
                indent=2,
            )
            f.write("\n")

    # Scanned letter: prose only (OCR accuracy + no false tables).
    img = Image.new("L", (1500, 120 + 56 * len(SCAN_LETTER) + 80), 255)
    d = ImageDraw.Draw(img)
    for i, line in enumerate(SCAN_LETTER):
        d.text((80, 100 + i * 56), line, font=font, fill=0)
    img.save(os.path.join(OUT, "scanned-letter.png"))
    with open(os.path.join(OUT, "scanned-letter.png.gt.json"), "w") as f:
        json.dump(
            {
                "mime": "image/png",
                "entities": [],
                "tables": [],
                "no_table_pages": [1],
                "ocr_text": "\n".join(SCAN_LETTER),
            },
            f,
            indent=2,
        )
        f.write("\n")


def main():
    os.makedirs(OUT, exist_ok=True)
    invoice_pdf()
    entities_txt()
    if "--no-raster" not in sys.argv:
        raster_images()
    print("wrote", os.path.normpath(OUT))


if __name__ == "__main__":
    main()
