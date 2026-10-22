# Changelog

## 0.1.5 — 2026-10-22

Not published. The tag `v0.1.5` is ready to push when the release is approved. This changelog is the release note.

### Requests

- Each LLM and Google call has a deadline of 60 seconds (`--timeout-secs`, JSON `timeout_secs`). A stalled socket is abandoned and retried with backoff, up to four attempts. A hung connection cannot hold the job open.

### Progress

- `rpt translate` writes progress to stderr: pages and segments done, segments per second, calls, and retries. A line that starts `waiting Ns` means no segment finished in that time. The rewrite phase prints `rewriting` and a line every five seconds, then `wrote` the output path. `--progress json` prints one JSON object per update. `--progress off` is quiet. The library default stays quiet.

### Memory and output

- Stdout JSON is a summary (page info, glyph count, coverage counts, translations, translator stats). `--glyphs` prints the per-glyph document on stdout. `--detail FILE` writes that document to a file.
- Bilingual output reuses the source image and font objects instead of cloning the PDF. Unreferenced page objects are dropped after the pages are composed.
- Coverage counts references, figures, tables, code, math, and names as intentional keeps. Only a real miss (an untranslated body paragraph, overflow, a missing glyph, a missing font, or an incomplete list) makes the document incomplete.

### Text

- Author names and bylines stay in Latin, including a long author line and a comma-separated author list.
- A title the model echoes, including one with an extra subtitle such as `性能基准测试`, is sent again. The extra subtitle is not painted.
- `--system-prompt` appends text, or the contents of a file, to the system prompt. `--glossary` accepts `source=target` or a CSV file with `src,tgt`. `--translate-tables` translates table cell text; the default is still the caption only.

### Release binaries

- `.github/workflows/release-cli.yml` builds `rpt` for Linux x86_64 and aarch64, glibc and musl. It uploads artifacts and does not publish a GitHub Release.

## 0.1.4 — 2026-10-22

Layout and formula painting since 0.1.3. The C/C++ SDK version is the Cargo workspace version.

### Formulas

- Display equations stay in their original glyphs. Inline math stays in the sentence as one atom, including a formula the sentence could not name and a placeholder inside parentheses.
- Subscripts and superscripts travel with their base. A leftover source glyph is not painted again. A closing brace the line groups skipped is written back. Equation digits stay on the baseline, and a subscript digit is not treated as a contents page number.
- Formula rules move or blank with the glyphs they belong to. A display sum no longer pins the paragraph in English. Short formulas sit on the line above, and a math-dense line keeps its superscripts.
- The Adam paragraph translates with `warmup_steps` intact.

### Tables, captions, and footnotes

- Table cells stay in the original text. Each caption is translated as one sentence and stays in its column.
- Each footnote is its own paragraph on its own baseline, and the footnote rule moves with the note. Numbered footnotes stay on their page.

### Type and coverage

- Chinese defaults to Songti, with titles in Song Bold. Subsets are built in Rust and stay paintable in Windows viewers. Each subset font has its own name.
- Body paragraphs stay flush left. Titles, centered notices, and author columns keep their alignment, and author marks hang off the name.
- Source numbers, decimals, and heading spaces stay intact. A sentence that crosses a page stays in Chinese. Institution names and Latin affiliations are not treated as unfinished paragraphs.
