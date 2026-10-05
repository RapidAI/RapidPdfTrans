# Translation benchmark

`rpt-bench` scores a translated PDF against the source. `corpus/bench/run.py` records a snapshot in `history.jsonl` and `latest.md`. The same metrics are used for RapidPdfTrans, BabelDOC, and PDFMathTranslate.

| Metric | What it measures |
| --- | --- |
| `line_coverage` / `drop_rate` | Source text lines whose box still meets an output glyph. A miss is dropped or moved text. |
| `protected_recall` | URLs, emails, `{braces}`, numbers, and `⟦N⟧` placeholders from the source that still occur in the output. |
| `formula_integrity` | Math-font runs kept as the same Unicode or as glyphs still on that box. |
| `overflow_rate` | Output glyphs that extend past the page box. |
| `style_retention` | Output glyphs that sit within 0.8 pt of a source glyph and match its size and bold/italic class. |
| `identity_char_retention` | Source characters still present in the output. This is the reference metric when every engine uses `corpus/bench/identity_server.py`. A real translation into another language lowers it on purpose. |
| non-text SSIM | Poppler render at 72 dpi. Blocks that are mostly source glyph boxes are excluded, so figures and rules are what remain. |
| `source_unmapped_ratio` | Source glyphs with no Unicode. They cannot be translated faithfully. On an identity run this is an extraction score, not an engine-output score. |

An identity run compares each file with itself. Drop rate, style, overflow, and non-text SSIM should be ideal. Unmapped glyphs are the extraction gap.

LLM-as-judge is not called unless a later run sets `RPT_LLM_API_KEY`. The key is not stored in these files. Until then, identity character retention is the reference metric for the shared identity translator.

External engines, when installed, should use that identity server so the translator is the same:

```bash
python3 corpus/bench/identity_server.py --port 8765
# BabelDOC
babeldoc --openai --openai-model identity --openai-base-url http://127.0.0.1:8765/v1 \
  --openai-api-key local --no-dual --pages 1 --files paper.pdf
# PDFMathTranslate
OPENAI_BASE_URL=http://127.0.0.1:8765/v1 OPENAI_API_KEY=local OPENAI_MODEL=identity \
  pdf2zh paper.pdf -s openai -o /tmp/pdf2zh-out
cargo run -p rpt-qa --bin rpt-bench -- pair --source paper.pdf --output translated.pdf
```

`python3 corpus/bench/run.py` refreshes the identity ceiling. It does not download BabelDOC or PDFMathTranslate.
