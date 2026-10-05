# Head-to-head translation benchmark

Recorded 2026-10-05T20:50:03Z on commit `853ad54`.

Each engine translated the same CI pages through `gateway https://hub.mypapers.top/api/llm/v1 model auto`, limited to the first 1 page(s). The model is the shared gateway (`auto`). `identity_char_retention` near 1.0 means the source characters survived; a real translation is expected to score lower there.

BabelDOC is 0.6.x (`babeldoc`). PDFMathTranslate is pdf2zh_next 2.9.0. Both use `--primary-font-family serif`, `--no-dual`, and no watermark. BabelDOC is also passed `--disable-same-text-fallback` so an identity reply is kept. pdf2zh_next has no such flag; identical paragraphs fall back to its one-line translator, which still echoes the source.

## Means

| engine | docs | drop | coverage | protected | formula | overflow | style | identity | containment | non-text SSIM |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `rapidpdftrans` | 3 | 0.0000 | 1.0000 | 1.0000 | 1.0000 | 0.0000 | 0.9858 | 0.0720 | 0.9900 | 0.9917 |
| `babeldoc` | 3 | 0.1346 | 0.8654 | 0.6414 | 0.8667 | 0.0000 | 0.9408 | 0.0553 | 0.9780 | 0.9802 |
| `pdf2zh_next` | 3 | 0.0333 | 0.9667 | 0.7323 | 1.0000 | 0.0000 | 0.9524 | 0.0681 | 0.9474 | 0.9792 |

## Per file

| id | engine | status | drop | formula | overflow | style | identity | containment |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `arxiv-2610.02163` | `rapidpdftrans` | ok | 0.0000 | 1.0000 | 0.0000 | 0.9893 | 0.0832 | 1.0000 |
| `arxiv-2610.02163` | `babeldoc` | ok | 0.2143 | 0.6000 | 0.0000 | 0.9717 | 0.0517 | 1.0000 |
| `arxiv-2610.02163` | `pdf2zh_next` | ok | 0.0306 | 1.0000 | 0.0000 | 0.9916 | 0.0846 | 0.9647 |
| `pmlr-v202-abbas23a` | `rapidpdftrans` | ok | 0.0000 | 1.0000 | 0.0000 | 0.9776 | 0.0564 | 0.9890 |
| `pmlr-v202-abbas23a` | `babeldoc` | ok | 0.0440 | 1.0000 | 0.0000 | 0.9085 | 0.0429 | 0.9559 |
| `pmlr-v202-abbas23a` | `pdf2zh_next` | ok | 0.0330 | 1.0000 | 0.0000 | 0.9220 | 0.0469 | 0.8971 |
| `neurips-2023-00296c0e` | `rapidpdftrans` | ok | 0.0000 | 1.0000 | 0.0000 | 0.9904 | 0.0765 | 0.9811 |
| `neurips-2023-00296c0e` | `babeldoc` | ok | 0.1455 | 1.0000 | 0.0000 | 0.9421 | 0.0712 | 0.9783 |
| `neurips-2023-00296c0e` | `pdf2zh_next` | ok | 0.0364 | 1.0000 | 0.0000 | 0.9437 | 0.0728 | 0.9804 |
