# Head-to-head translation benchmark

Recorded 2026-10-05T19:26:11Z on commit `32f8f77`.

Each engine translated the same CI pages through the identity server (`identity http://127.0.0.1:8765/v1`), limited to the first 1 page(s). The translator returns the source text, so these numbers measure layout damage rather than translation quality. `identity_char_retention` near 1.0 means the source characters survived.

BabelDOC is 0.6.x (`babeldoc`). PDFMathTranslate is pdf2zh_next 2.9.0. Both use `--primary-font-family serif`, `--no-dual`, and no watermark. BabelDOC is also passed `--disable-same-text-fallback` so an identity reply is kept. pdf2zh_next has no such flag; identical paragraphs fall back to its one-line translator, which still echoes the source.

## Means

| engine | docs | drop | coverage | protected | formula | overflow | style | identity | containment | non-text SSIM |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `rapidpdftrans` | 9 | 0.0000 | 1.0000 | 1.0000 | 1.0000 | 0.0000 | 1.0000 | 1.0000 | 1.0000 | 1.0000 |
| `babeldoc` | 9 | 0.0346 | 0.9654 | 0.9238 | 1.0000 | 0.0000 | 0.9740 | 0.9998 | 0.9750 | 0.9843 |
| `pdf2zh_next` | 9 | 0.1651 | 0.8349 | 0.8969 | 1.0000 | 0.0000 | 0.1137 | 0.9998 | 0.8345 | 0.9516 |

## Per file

| id | engine | status | drop | formula | overflow | style | identity | containment |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `arxiv-2610.02163` | `rapidpdftrans` | ok | 0.0000 | 1.0000 | 0.0000 | 1.0000 | 1.0000 | 1.0000 |
| `arxiv-2610.02163` | `babeldoc` | ok | 0.0102 | 1.0000 | 0.0000 | 0.9702 | 1.0000 | 0.9894 |
| `arxiv-2610.02163` | `pdf2zh_next` | ok | 0.0612 | 1.0000 | 0.0000 | 0.0780 | 1.0000 | 0.9633 |
| `pmlr-v202-abbas23a` | `rapidpdftrans` | ok | 0.0000 | 1.0000 | 0.0000 | 1.0000 | 1.0000 | 1.0000 |
| `pmlr-v202-abbas23a` | `babeldoc` | ok | 0.0330 | 1.0000 | 0.0000 | 0.8776 | 1.0000 | 0.9444 |
| `pmlr-v202-abbas23a` | `pdf2zh_next` | ok | 0.5385 | 1.0000 | 0.0000 | 0.0040 | 1.0000 | 0.8000 |
| `neurips-2023-00296c0e` | `rapidpdftrans` | ok | 0.0000 | 1.0000 | 0.0000 | 1.0000 | 1.0000 | 1.0000 |
| `neurips-2023-00296c0e` | `babeldoc` | ok | 0.0545 | 1.0000 | 0.0000 | 0.9958 | 1.0000 | 0.9649 |
| `neurips-2023-00296c0e` | `pdf2zh_next` | ok | 0.0545 | 1.0000 | 0.0000 | 0.1472 | 1.0000 | 0.7500 |
| `arxiv-2610.01998` | `rapidpdftrans` | ok | 0.0000 | 1.0000 | 0.0000 | 1.0000 | 1.0000 | 1.0000 |
| `arxiv-2610.01998` | `babeldoc` | ok | 0.0180 | 1.0000 | 0.0000 | 0.9808 | 1.0000 | 1.0000 |
| `arxiv-2610.01998` | `pdf2zh_next` | ok | 0.0270 | 1.0000 | 0.0000 | 0.1778 | 1.0000 | 0.9173 |
| `pmlr-v202-abels23a` | `rapidpdftrans` | ok | 0.0000 | 1.0000 | 0.0000 | 1.0000 | 1.0000 | 1.0000 |
| `pmlr-v202-abels23a` | `babeldoc` | ok | 0.0101 | 1.0000 | 0.0000 | 0.9977 | 1.0000 | 0.9899 |
| `pmlr-v202-abels23a` | `pdf2zh_next` | ok | 0.1313 | 1.0000 | 0.0000 | 0.0041 | 1.0000 | 0.8741 |
| `neurips-2017-attention` | `rapidpdftrans` | ok | 0.0000 | 1.0000 | 0.0000 | 1.0000 | 1.0000 | 1.0000 |
| `neurips-2017-attention` | `babeldoc` | ok | 0.0938 | 1.0000 | 0.0000 | 0.9492 | 0.9984 | 0.9242 |
| `neurips-2017-attention` | `pdf2zh_next` | ok | 0.2969 | 1.0000 | 0.0000 | 0.0031 | 0.9984 | 0.4286 |
| `2024-acl-long-7` | `rapidpdftrans` | ok | 0.0000 | 1.0000 | 0.0000 | 1.0000 | 1.0000 | 1.0000 |
| `2024-acl-long-7` | `babeldoc` | ok | 0.0180 | 1.0000 | 0.0000 | 0.9975 | 1.0000 | 0.9727 |
| `2024-acl-long-7` | `pdf2zh_next` | ok | 0.1982 | 1.0000 | 0.0000 | 0.1017 | 1.0000 | 0.8986 |
| `arxiv-2610.02193` | `rapidpdftrans` | ok | 0.0000 | 1.0000 | 0.0000 | 1.0000 | 1.0000 | 1.0000 |
| `arxiv-2610.02193` | `babeldoc` | ok | 0.0526 | 1.0000 | 0.0000 | 1.0000 | 1.0000 | 1.0000 |
| `arxiv-2610.02193` | `pdf2zh_next` | ok | 0.1474 | 1.0000 | 0.0000 | 0.0871 | 1.0000 | 0.9802 |
| `arxiv-2609.36965` | `rapidpdftrans` | ok | 0.0000 | 1.0000 | 0.0000 | 1.0000 | 1.0000 | 1.0000 |
| `arxiv-2609.36965` | `babeldoc` | ok | 0.0208 | 1.0000 | 0.0000 | 0.9971 | 1.0000 | 0.9894 |
| `arxiv-2609.36965` | `pdf2zh_next` | ok | 0.0312 | 1.0000 | 0.0000 | 0.4204 | 1.0000 | 0.8981 |
