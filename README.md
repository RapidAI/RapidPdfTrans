# RapidPdfTrans

Format-preserving PDF translation. The goal is to replace [BabelDOC](https://github.com/funstory-ai/BabelDOC): keep the layout, and never silently drop text.

It opens a PDF, interprets content streams down to individual glyphs, and checks that every glyph is still accounted for. A pluggable translator marks those glyphs as translated. `rpt translate` without `--output` reports that translation and does not write a file. `rpt translate --output` rewrites the glyphs into a PDF and removes the original text-showing operators for the glyphs it rewrote.

The Chinese page is [README.zh.md](README.zh.md).

## Usage

### Install

Rust stable (1.88 or newer; the workspace pins `stable`):

```bash
cargo build -p rpt-cli --release
./target/release/rpt extract testdata/hello.pdf --compact
```

`rpt extract` prints JSON to stdout and does not write a PDF. On `testdata/hello.pdf` the plain text is `Hello` and the coverage object has `"pending": 5`.

Python (needs `python3-dev`). The module calls the Rust core directly:

```bash
cargo build -p rpt-python --release
cp target/release/librapidpdftrans.so rapidpdftrans.so
python3 -c 'import rapidpdftrans; print(rapidpdftrans.version()); print("Hello" in rapidpdftrans.extract("testdata/hello.pdf"))'
```

`rapidpdftrans.translate(path)` returns JSON and does not write a file. `rapidpdftrans.save(path, output)` writes a PDF.

C++ is a header-only wrapper over the C ABI:

```bash
cargo build -p rpt-ffi --release
g++ -std=c++17 -I include -I bindings/cpp bindings/cpp/smoke.cpp \
  target/release/librapidpdftrans.a -ldl -lm -lpthread -lgcc_s -o smoke
./smoke testdata/hello.pdf
```

`rpt::Document::extract` returns JSON. `save` writes a PDF and throws when no translator is configured and Google Translate is unreachable.

Go, from `bindings/go`, after that static library exists:

```bash
go test ./...
```

```go
text, err := rapidpdftrans.Extract("testdata/hello.pdf")
```

`Translate` returns JSON and does not write a PDF. The C function that writes a file is `rpt_save`.

### Environment

Set the key in the environment. An `api_key` field in options JSON is ignored.

| Variable | When | Default |
| --- | --- | --- |
| `RPT_LLM_API_KEY` | live LLM translation | unset |
| `RPT_LLM_BASE_URL` | optional LLM gateway | `https://hub.mypapers.top/api/llm/v1` |
| `RPT_LLM_MODEL` | optional LLM model | `auto` |
| `RPT_TRANSLATOR` | optional backend name | unset (see below) |
| `RPT_GOOGLE_API_KEY` | Cloud Translation v2 | unset |
| `RPT_GOOGLE_CREDENTIALS` | `google-v3` service account | unset |
| `RPT_GOOGLE_ACCESS_TOKEN` | `google-v3` bearer token | unset |
| `RPT_CJK_FONT` | optional Song/serif file | Noto Serif CJK, then the fallback list below |
| `RPT_CJK_SANS` | optional sans file | Noto Sans CJK |
| `RPT_TRANSLATE_JOBS` | optional parallel requests | `4` (cap `8`) |
| `RPT_CJK_SIZE_SCALE` | optional | `0.90` |
| `RPT_CJK_LEADING` | optional | `1.60` |

`RPT_TRANSLATOR` is `maclaw`, `openai`, `google`, `google-v2`, `google-v3`, or `google-unofficial`. When it is unset and no `RPT_LLM_API_KEY`, `RPT_LLM_BASE_URL`, `RPT_LLM_MODEL`, `model`, or `base_url` is set, the order is: `RPT_GOOGLE_API_KEY` (Cloud Translation v2), then v3 credentials (`RPT_GOOGLE_CREDENTIALS` or `RPT_GOOGLE_ACCESS_TOKEN`), then a 3-second probe of `https://translate.googleapis.com/translate_a/single?client=gtx`. A successful probe uses that endpoint. If the probe fails, translation stops and asks for `RPT_LLM_API_KEY` or `RPT_TRANSLATOR`. An explicit translator name or LLM setting skips the probe.

### Fonts

`--cjk-font` and `--cjk-sans` take a TTF, TTC, or OTF. The same paths can be given as `RPT_CJK_FONT` and `RPT_CJK_SANS`. Bold text still uses `/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc` (collection face 2) when that file is installed.

When the serif path is unset, body text uses Noto Serif CJK SC Light if it is installed (`~/.local/share/fonts/NotoSerifCJKsc-Light.otf`, `/usr/local/share/fonts/NotoSerifCJKsc-Light.otf`, or `/usr/share/fonts/opentype/noto/NotoSerifCJKsc-Light.otf`), otherwise `/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc` (face 2). Sans text uses `/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc` (face 2). If those faces cannot be subset, the fallback is `RPT_CJK_FONT` when it is set, then `/usr/share/fonts/truetype/wqy/wqy-microhei.ttc`, `/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf`, and `NotoSansCJK-Regular.ttc` under `/usr/share/fonts/opentype/noto` or `/usr/share/fonts/truetype/noto`.

### Translate

```bash
export RPT_LLM_API_KEY=...
./target/release/rpt translate paper.pdf --from en --to zh --output paper.zh.pdf
```

Flags that exist on `rpt translate`:

| Flag | Meaning |
| --- | --- |
| `--output` | Write the translated PDF. Without it, no PDF is written. |
| `--from`, `--to` | Source and target language. Default `en` and `zh`. |
| `--mode` | `replace` (default), `bilingual`, `side-by-side`, `alternating`, or `overlay`. |
| `--bilingual` | Bilingual output. Layout is `--layout`. |
| `--layout` | `side-by-side` (default), `alternating`, or `overlay`. Used with `--bilingual` or `--mode bilingual`. |
| `--bilingual-output` | Also write a bilingual PDF from the same translation. |
| `--max-pages N` | Only the first N pages. |
| `--model`, `--base-url` | Override `RPT_LLM_MODEL` and `RPT_LLM_BASE_URL` for this run. |
| `--glossary source=target` | Repeat for more than one term. Applied locally before the call. |
| `--translate-references` | Translate the bibliography. Default is to keep it. |
| `--translate-figures` | Translate text inside figures. Default is the caption only. |
| `--translate-tables` | Translate cells and headers. Default is the caption only. |
| `--cjk-font`, `--cjk-sans` | TTF, TTC, or OTF. |
| `--cjk-size-scale`, `--cjk-leading` | Default `0.90` and `1.60`. |
| `--batch-size` | Segments per model call. Default `8`. |
| `--jobs` | Parallel model requests. Default `4`, or `RPT_TRANSLATE_JOBS`, capped at `8`. Layout stays single-threaded. |
| `--replay file.json` | Reuse a previous translation JSON. No model call. |
| `--compact` | One-line JSON on stdout. |
| `--json` | Accepted for compatibility. JSON is always written to stdout. |

`--json` does not select a file. After `--output` succeeds, the process also prints the extraction JSON, including a `translations` array and a `translator` object (`model`, `backend`, `base_url`, `calls`, `cache_hits`).

### Example output

`testdata/hello.pdf` extracts as plain text `Hello`. Coverage is not complete, because nothing has been translated:

```json
{"complete": false, "pending": 5, "total": 5}
```

With no LLM settings and Google Translate unreachable, translate exits 1 and writes nothing:

```text
rpt: translation failed: no translator is configured and Google Translate is unreachable; set RPT_LLM_API_KEY or RPT_TRANSLATOR
```

An explicit `RPT_TRANSLATOR=maclaw` with no key still reports that `RPT_LLM_API_KEY` is not set.

### Troubleshooting

**Missing translator.** With nothing configured, RapidPdfTrans probes Google Translate. Export `RPT_LLM_API_KEY` before `rpt translate`, Python `save` / `translate`, C++ `save`, or `rpt_save` to use the LLM gateway instead. A key placed in options JSON is ignored.

**Tofu (empty boxes).** The chosen CJK face has no outline for that character. The glyph stays original with reason `missing-glyph`. Pass a face that contains it:

```bash
./target/release/rpt translate paper.pdf --from en --to zh \
  --cjk-font /usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc \
  --output paper.zh.pdf
```

`RPT_CJK_FONT` is the same path. A path that is not a file fails before any request: `rpt: CJK font not found: ...`.

## Layout

| Path | Role |
| --- | --- |
| `crates/rpt-core` | Interpreter, font decoding, glyph coverage, translator |
| `crates/rpt-ffi` | C ABI (`rpt_open`, `rpt_extract`, `rpt_translate`, `rpt_save`, `rpt_free`) |
| `crates/rpt-cli` | `rpt extract` and `rpt translate` |
| `crates/rpt-qa` | Corpus fidelity report (`rpt-qa`) |
| `crates/rpt-python` | Native Python module via PyO3 (not the C ABI) |
| `include/rapidpdftrans.h` | C header generated by cbindgen |
| `bindings/cpp` | Header-only RAII wrapper |
| `bindings/go` | cgo wrapper over the static library |
| `testdata/hello.pdf` | One-page fixture for the binding smoke tests |

Coordinates are PDF user space: origin at the bottom left, y increasing upward (`pdf-user-space-origin-bottom-left-y-up`). Page `/Rotate` is reported on each page and is not applied to glyph positions. `q` / `Q` save the graphics state and the text state (`Tc` `Tw` `Tz` `TL` `Ts` `Tf` `Tr`). They do not save the text matrix, matching the PDF specification.

## Build

Rust stable (1.88 or newer; the workspace pins `stable`):

```bash
cargo test --workspace
cargo build -p rpt-cli --release
./target/release/rpt extract testdata/hello.pdf --json
```

`rpt extract` writes per-glyph JSON, a plain-text reconstruction, and a coverage report to stdout. `rpt translate` without `--output` translates that text and does not write a PDF. Pass `--output` to write the translated PDF.

Python (needs `python3-dev`):

```bash
cargo build -p rpt-python --release
cp target/release/librapidpdftrans.so rapidpdftrans.so
python3 -c 'import rapidpdftrans; print(rapidpdftrans.extract("testdata/hello.pdf")[:200])'
```

C ABI, used by Go and C++:

```bash
cargo build -p rpt-ffi --release
# header: include/rapidpdftrans.h
# static library: target/release/librapidpdftrans.a
g++ -std=c++17 -I include -I bindings/cpp bindings/cpp/smoke.cpp \
  target/release/librapidpdftrans.a -ldl -lm -lpthread -lgcc_s -o smoke
```

The C shared library is the same package, requested explicitly so a workspace build does not overwrite the Python module (both use the filename `librapidpdftrans.so`):

```bash
cargo rustc -p rpt-ffi --release -- --crate-type cdylib
# Linux: target/release/librapidpdftrans.so
# macOS: target/release/librapidpdftrans.dylib
# Windows: target/release/rapidpdftrans.dll
```

Go (from `bindings/go`, after the release static library exists):

```bash
go test ./...
```

`rpt_save` translates the open document and writes a PDF. It returns -1, and does not create the file, when no translator is configured and Google Translate is unreachable.

## Prebuilt packages

`.github/workflows/sdk.yml` archives the headers, the shared library, and the static library for each platform, with a CMake config and a Unix pkg-config file. `.github/workflows/wheels.yml` builds CPython 3.10+ abi3 wheels with cibuildwheel (one wheel per platform loads on CPython 3.10 and newer). Both workflows run on pull requests, `workflow_dispatch`, and tags `v*`. Pull requests upload build artifacts only. A tag whose name matches `workspace.package.version` in `Cargo.toml` (`v0.1.3` for version `0.1.3`) attaches the archives, wheels, and sdist to a GitHub Release and publishes the wheels and sdist to PyPI.

32-bit macOS is not built: there is no hosted runner for it, and current CPython does not ship a 32-bit macOS installer. musllinux wheels are built for x86_64 and aarch64. Rust does not publish a host compiler for `i686-unknown-linux-musl` or `armv7-unknown-linux-musleabihf`, and cibuildwheel compiles inside that container. Intel macOS uses `macos-15-intel`; GitHub no longer provides `macos-13`. PyPI upload uses the `PYPI_APIKEY` repository secret (a PyPI API token) from the tag's wheel workflow, with `skip-existing` set. It does not use trusted publishing.

## Translator

The default backend is an OpenAI-compatible chat endpoint:

- base URL `https://hub.mypapers.top/api/llm/v1` (`RPT_LLM_BASE_URL` or the `base_url` option)
- model `auto` (`RPT_LLM_MODEL` or the `model` option)
- API key from `RPT_LLM_API_KEY` only

An `api_key` field in options JSON is ignored. Explicit options win over environment variables, which win over the built-in defaults. Tests talk to an in-process mock server. The live test runs only when `RPT_LLM_API_KEY` is set.

`RPT_TRANSLATOR` selects the backend: `maclaw`, `openai`, `google` / `google-v2` (`RPT_GOOGLE_API_KEY`), `google-v3` (a service account in `RPT_GOOGLE_CREDENTIALS` or a bearer token in `RPT_GOOGLE_ACCESS_TOKEN`), or `google-unofficial` (`translate.googleapis.com/translate_a/single?client=gtx`). Explicit config wins over a configured LLM, which wins over Google. With nothing configured, a successful 3-second probe selects the unofficial endpoint. Placeholders `⟦N⟧` are sent as `XPHNX` on that endpoint and restored afterwards. Requests are batched, at most 4 run at once, and HTTP 429/5xx are retried with backoff.

```bash
export RPT_LLM_API_KEY=...
./target/release/rpt translate paper.pdf --from en --to zh --output paper.zh.pdf
```

With `RPT_LLM_API_KEY` set, that command posts to `RPT_LLM_BASE_URL` (`https://hub.mypapers.top/api/llm/v1` when unset) with model `RPT_LLM_MODEL` (`auto` when unset). The key is only `RPT_LLM_API_KEY`. With no LLM settings and no `RPT_TRANSLATOR`, the same command uses Google Translate when the probe succeeds. `--glossary transformer=Transformer` is optional and is applied locally before the call. Parallel requests default to 4.

CJK glyphs are embedded from a face on the machine. `RPT_CJK_FONT` is the Song/serif file and `RPT_CJK_SANS` is the sans file. When those are unset, bold text uses `/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc` (collection face 2), serif text uses Noto Serif CJK SC Light when that file is installed and otherwise `/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc` (face 2), and sans text uses `/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc` (face 2). When those faces cannot be subset, the fallback is `RPT_CJK_FONT` if it is set, then `/usr/share/fonts/truetype/wqy/wqy-microhei.ttc`, `/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf`, and `NotoSansCJK-Regular.ttc` under `/usr/share/fonts/opentype/noto` or `/usr/share/fonts/truetype/noto`. The embedded font is a glyf subset (Identity-H, ToUnicode). Horizontal advances come from `hmtx`; OpenType GSUB is not applied.

The default output is pure target-language text: the translation replaces the original in place. Bilingual output is opt-in. `--mode side-by-side` puts the original page on the left and the translation on the right. `--mode alternating` writes the original page, then the translated page. `--mode overlay` keeps the original operators and draws the translation on the same page. `--bilingual` selects side-by-side unless `--layout alternating` or `--layout overlay` is set. `--layout` alone does not change the default. The same choice is `output_mode` (default `replace`) and, only when bilingual is requested, `bilingual_layout` in options JSON for `rpt_save`, the C++ `save`, and Python `save`. A layout field by itself leaves the PDF as pure translation. `--max-pages N` limits extraction to the first N pages. A segment that does not fit, or that shares an operator with a formula or another kept glyph, stays original.

Before the call, URLs, email addresses, `{brace}` groups, numbers, in-text citations (`[12]`, `[1-3]`, `(Smith et al., 2020)`, `Smith (2019)`), and glossary terms are replaced with `⟦N⟧` placeholders and restored afterwards. Glossary terms are substituted locally (longest match, ASCII word boundaries), so the model does not have to obey a glossary table. A dropped placeholder is retried once and then reported as an error.

The References / Bibliography section is not translated. Headings such as `References`, `Bibliography`, `Works Cited`, `参考文献`, `Literatur`, and `Références` start the section; numbered and author-year entries do too. It continues across columns and pages and stops at the next section (`Appendix`, `附录`, `A. Proofs`, …). Those glyphs are `kept_original` with reason `references`. `skip_references` defaults to true; set it to false in options JSON, or pass `--translate-references` on the CLI, to translate the section.

`rpt translate --output` then rewrites those glyphs. Without `--output` they stay `translated_pending_rewrite`.

## Coverage

Every extracted glyph starts as `pending`. A document is complete only when each glyph has exactly one final disposition:

- `rewritten` — translated text has been written back into the PDF
- `kept_original` — the original drawing is kept, with a reason
- `non_text` — classified as non-text, with a reason

`translated_pending_rewrite` records a translation and stays unresolved until rewrite. Extraction alone therefore reports every glyph as unresolved. A test fails if an extraction path skips a glyph and shrinks that list.

Glyph records include the page, Unicode (which may be several characters, or empty when unmapped), character-code bytes, GID when known, font name and size, the text rendering matrix, an approximate bounding box, advance, colors, render mode, invisible and clip flags, and the source location (content stream, Form XObject, annotation appearance, or Type3 char proc, plus operator index and byte range). `rpt translate --output` uses that location to delete the original text-showing operator for each rewritten glyph.

## What the interpreter covers

- Graphics state: `q` / `Q`, `cm`, text state, `Tm` / `Td` / `TD` / `T*` / `'` / `"`, and `TJ` kerning
- Widths from simple `/Widths` and CID `/W` / `/DW`; vertical writing via `/W2` / `/DW2`
- Form XObjects (matrix, resources, inheritance, cycles) and annotation `/AP` normal appearances
- Type3 char procs, including text drawn inside them
- Inline images, without aborting the page on a bad operator
- Invisible text (`Tr` 3 or 7) is recorded and flagged, not dropped
- Rectangular clips flag glyphs that fall outside; other clips set `clip_uncertain`
- Unicode fallback: ToUnicode CMap, then predefined CJK CMaps (decoded with `encoding_rs`, not Adobe's CID tables), then simple encodings and `/Differences` via the Adobe Glyph List, then glyph names from a Type 1 `FontFile` cleartext encoding (`dup 65 /A put`, including PFB), then the embedded font cmap. Unmapped glyphs are kept and flagged.

## Fidelity corpus

`corpus/manifest.json` lists real papers and openly licensed books (direct URL, source, license, sha256, size, feature tags). `python3 corpus/fetch.py` downloads them into a gitignored cache. Those files are not committed. `corpus/ci/` is the exception used by CI: nine CC BY 4.0 papers, each under 700 KB, with URLs and licenses in `corpus/ci/manifest.json`. `testdata/hello.pdf` is a one-page file generated by `cargo run -p rpt-core --example hello_pdf`, not a corpus document. `rpt-qa` extracts every cached file, checks glyph coverage, compares text with Poppler, and render-diffs an identity byte copy (SSIM on the page and on non-text blocks). A lopdf save is reported separately because it rewrites the file. CI runs the small `ci: true` subset. The full corpus is the nightly `Corpus fidelity` workflow. See `corpus/README.md`.

## Translation benchmark

`rpt-bench` scores a translated PDF on dropped lines, placeholder and formula retention, overflow, style, non-text SSIM, and reference-operator byte identity. `python3 corpus/bench/run.py` records the identity ceiling (each CI PDF compared with itself) in `corpus/benchmarks/`. BabelDOC and PDFMathTranslate, when installed, should share `corpus/bench/identity_server.py` so the translator is the same. Metric definitions are in `corpus/benchmarks/README.md`.

## Roadmap

1. **M1.** Content-stream interpreter, glyph coverage, pluggable translator, C / Python / Go / C++ skeletons, CLI.
2. **M2.** Layout analysis, placeholder protection beyond the current shield, translator caching and glossary integration in the layout model.
3. **M3 (started).** Re-layout with `hmtx` advances, CJK line breaking, font subsetting, deletion of the original text operators, bilingual mode, and a working `rpt_save`. GSUB shaping and full paragraph layout are still open.
4. **M4.** Richer layout, OCR for pages that are not real text, render-diff checks against PDFium.

## Decisions worth weighing

- For a non-Identity CJK CMap, `/W` is looked up with the character code itself, not an Adobe-GB1 / Japan1 CID. `/DW` applies when that code is absent. Identity-H/V uses the numeric code as the CID.
- Standard 14 fonts without `/Widths` use `MissingWidth` (default 0). AFM metrics are not vendored, so those advances are wrong until the PDF embeds widths.
- Rectangle clips are exact enough to flag glyphs. Arbitrary paths only set `clip_uncertain`.
- A Type3 character records both the Type3 glyph and any text-showing operators inside its char proc.
- Glyph boxes are an em approximation (descent 0.2, ascent 0.8), not ink bounds.
- ZapfDingbats encoding is not included.
- PDFium is not a runtime dependency. A cross-check test was not added because libpdfium is not assumed to be installed.
- The in-memory translation cache is per call, keyed by the shielded source string.
