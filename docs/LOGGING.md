# Server logs

`aikoql-mcp serve` logs to **stderr and to a file**, so what happened is still on disk after a restart,
a crash or a closed terminal.

| | |
|---|---|
| Default file | `<database path>.server.log` (next to the database, like `<db>.audit.log`) |
| Change it | `[logging] file = "/var/log/aikoql/server.log"` in `aikoql.toml`, or `AIKOQL_LOG_FILE` (env wins) |
| Turn it off | `file = "off"` (or empty): stderr only |
| Level / format | `[logging] level` (`trace`…`error`), `format` (`text`/`json`), or `RUST_LOG` |
| Rotation | by size: at 50 MB the file becomes `.1`, `.1` becomes `.2`, … up to `.5` (oldest dropped) |
| Restart | appends; nothing is truncated |
| Colour codes | stripped from the file |
| Panics | logged (`the server panicked`) before the process dies |

The directory is created if missing. If the file cannot be opened the server says so on stderr and keeps
running with stderr only. Log text never contains source connection strings or passwords (the same
scrubbing as responses; the live multi-database tests assert it over every file under the server root).

**Not yet in the file:** output written with a raw `eprintln!` (about 190 places in the server and 26 in
the ingestion crate) still reaches stderr only; they are being moved to the logger as they are touched.
Document and OCR failures are in the file (`OCR failed for <file>, page N: <reason>`).

## Documents

- **OCR failures say why.** An ingest response carries `ocr_stats.failures` (the first five
  `page N: <reason>` lines), and Studio shows them under the upload message.
- **Compile results are bounded.** `document_compile` / `POST /api/v1/documents/compile` take
  `limit` (items per list, default 500) and `max_text` (characters per text field, default 2000).
  Lists are cut at every depth (arrays of plain numbers, such as embedding vectors, are never cut), and
  `truncated` reports each cut as `{shown, total}` plus `long_text: {fields, max_chars}`. A 1,446-page
  book compiled to 83 MB before this. Studio asks for `limit=100, max_text=1000` (about 2 MB); raise
  both through the API for the full result.
