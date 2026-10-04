# Fidelity corpus

Real PDFs used to check that extraction keeps every glyph and that the QA harness can see layout and style. The files themselves are not committed. `manifest.json` records the URL, source, license, sha256, size, and feature tags. `fetch.py` downloads them into `cache/`, which is gitignored.

```bash
python3 corpus/fetch.py          # full corpus, verifies sha256
python3 corpus/fetch.py --ci     # the small subset used by GitHub Actions
cargo run -p rpt-qa -- --render-pages 2
cargo run -p rpt-qa -- --ci --render-pages 1
```

`rpt-qa` writes `corpus/reports/summary.md` and `summary.json`.

What it checks, on every cached file:

- Open and extract without a panic. Glyph count, unmapped glyphs, diagnostics, and the coverage report (everything starts `pending`).
- Identity round-trip: the same bytes are written to a second path. Glyph text, positions, font name, size, fill color, and render mode must match. Poppler `pdftoppm` renders the first pages at 72 dpi; the report records exact-pixel ratio, full-page SSIM, and SSIM on blocks that are not mostly glyph boxes.
- Structural rewrite: lopdf loads the file and saves it again. Differences are reported. This is not translated output. Files larger than 25MB are skipped and counted separately from rewrite failures.
- Text cross-check against Poppler `pdftotext` (PDFium is not installed). The score is a whitespace-insensitive character multiset overlap, because our plain text does not invent spaces the PDF never drew. Both directions are reported: our characters found in Poppler, and Poppler characters found in our extraction.

Documents longer than 40 pages are extracted for the first 12 pages so a textbook cannot dominate the run. `pdf_pages` in the report is still the full count. `corpus/reports/summary.md` is the snapshot from the last full run.

Licenses are stored per file. Conference and arXiv PDFs are linked, not redistributed in git. Books in the manifest are openly licensed or public domain. `--discover` rebuilds the URL list from those public sources and replaces the manifest; the committed manifest is the frozen corpus.
