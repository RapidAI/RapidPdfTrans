---
name: rapidpdftrans
description: Translate a PDF (usually an English paper) into Chinese while keeping the original layout, formulas, tables, and references, using the RapidPdfTrans `rpt` CLI. Use when the user asks to translate a PDF to Chinese, make a Chinese or bilingual (中英对照) version of a paper, or check a translated PDF for layout problems.
---

# RapidPdfTrans: layout-preserving PDF translation

`rpt translate` reads a PDF, translates the body text with an OpenAI-compatible
LLM, and writes a new PDF with the Chinese text in the original positions.

## 1. Make sure `rpt` is available

```bash
command -v rpt || ls ./target/release/rpt
```

If neither exists, install it (pick one):

```bash
# Prebuilt Linux binary (x86_64; use rpt-linux-aarch64.tar.gz on ARM)
mkdir -p ~/.local/bin
curl -fsSL https://github.com/RapidAI/RapidPdfTrans/releases/latest/download/rpt-linux-x86_64.tar.gz \
  | tar -xz -C ~/.local/bin rpt

# Or build from source (Rust 1.88+), inside a clone of the repo
cargo build -p rpt-cli --release   # binary: ./target/release/rpt
```

The release binary can lag behind `main`; build from source for the newest fixes.

## 2. LLM settings come from the environment

Do not pass keys on the command line and never print them.

| Variable | Meaning |
| --- | --- |
| `OPENAI_API_KEY` | API key |
| `OPENAI_BASE_URL` | OpenAI-compatible endpoint, e.g. `https://host/v1` |
| `OPENAI_MODEL` | model name (the currently selected model) |

`RPT_LLM_API_KEY` / `RPT_LLM_BASE_URL` / `RPT_LLM_MODEL` are the project's
own names. When any `RPT_LLM_*` variable is set, that group is used and the
`OPENAI_*` group is ignored. `--base-url` and `--model` override both. With
`OPENAI_API_KEY`, a missing `OPENAI_BASE_URL` or `OPENAI_MODEL` stops the run
with an error naming the variable.

Release binaries up to v0.1.8 read only `RPT_LLM_*`. With one of those, map
the variables first (values stay in the environment):

```bash
export RPT_LLM_API_KEY="$OPENAI_API_KEY" RPT_LLM_BASE_URL="$OPENAI_BASE_URL" RPT_LLM_MODEL="$OPENAI_MODEL"
```

Check without revealing values:

```bash
for v in OPENAI_API_KEY OPENAI_BASE_URL OPENAI_MODEL; do
  [ -n "${!v}" ] && echo "$v set" || echo "$v MISSING"; done
```

## 3. Translate

```bash
rpt translate input.pdf --output input.zh.pdf
```

- Default `--from en --to zh`. Without `--output` no PDF is written.
- Default output is Chinese only (translation replaces the original text).
- Bilingual: add `--bilingual` (side-by-side page pairs), or
  `--bilingual --layout alternating|overlay`. To get both files from one
  translation: `--output a.zh.pdf --bilingual-output a.bilingual.pdf`.
- Parallel requests: `--jobs N` (default 4, or `RPT_TRANSLATE_JOBS`, cap 8).
  Use `--jobs 1` for a slow or rate-limited endpoint. Layout and PDF writing
  are always single-threaded.
- Kept in the source language by default: the References section
  (`--translate-references` to translate it), text inside figures
  (`--translate-figures`), table cells (`--translate-tables`; captions and
  column headers are translated), and formulas, code, URLs, numbers and
  citations (always kept).
- Quick try on a long paper: `--max-pages 3`.
- Long runs: `--progress off` for quiet logs, `--max-translate-secs N` for a
  wall-time budget, `--timeout-secs N` per request (default 60).
- Glossary: `--glossary transformer=Transformer` (repeatable, or a CSV file).
- Font: `--cjk-style serif` (default, Songti) or `--cjk-style sans` (Heiti).
  The Noto CJK fonts download to a cache on first use if not installed.

Save the stdout JSON so the run can be checked and replayed:

```bash
rpt translate input.pdf --output input.zh.pdf --compact > input.zh.json
```

`--replay input.zh.json` re-renders from saved translations with no model call.

## 4. Check the result

- Exit code: `0` PDF written; `1` failure (message on stderr starts with
  `rpt:`); `2` PDF written but the `--max-translate-secs` budget ran out, so
  some paragraphs stayed in English.
- Stderr ends with one line: `rpt: summary elapsed=… segments=… kept_source=… calls=…`.
- Layout QA: lines `rpt: warning: layout check: …` on stderr, and the
  `layout_warnings` array in the JSON (overlaps, wide gaps, missing glyphs).
  Add `--strict` to make those exit 1.
- Coverage: the JSON `coverage` object. `real_misses` > 0 or
  `english_body` > 0 means body text was left untranslated; `complete: true`
  means every glyph is accounted for. `intentional` / `kept_original` cover
  references, formulas, tables and names kept on purpose.
- Per-paragraph text is in `translations` (`source`, `translated`, `page_index`).
- Look at a few pages yourself (e.g. `pdftoppm -r 80 -f 1 -l 2 -png input.zh.pdf /tmp/pg`)
  before handing the PDF over.

## 5. Report back

Give the output path (the `--output` file; the bilingual file too if made),
the exit code, the summary line, and any layout warnings or untranslated
counts. Do not include API keys or endpoint credentials.
