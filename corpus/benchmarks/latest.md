# Translation benchmark

## Rewrite round, 2026-10-05

`rpt translate --output` now writes a PDF. Original text-showing operators are replaced with the same number of spaces. The translation is drawn with a subset Identity-H CID font. Reference operators are left untouched. Word spaces that were only gaps in the source are inserted before translation. Horizontal advances come from `hmtx`. OpenType GSUB is not applied.

`RPT_LLM_API_KEY` was not set in this environment, so this round used an identity translator (the extracted line, including rebuilt spaces, is drawn back). That measures rewrite fidelity, not translation quality. Both runs are the first page only.

| document | lines | drop | protected | formula | overflow | style | identity chars | non-text SSIM | page SSIM |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `arxiv-2610.02163` page 1 | 94 | 0.0000 | 0.8182 | 1.0000 | 0.0000 | 0.9920 | 0.9988 | 0.9975 | 0.7377 |
| `arxiv-2609.15979` page 1 | 76 | 0.0000 | 1.0000 | 1.0000 | 0.0000 | 0.9943 | 0.9996 | 0.9954 | 0.8221 |

No line was dropped. Formulas on these pages stayed original. Non-text pixels stayed put. Page SSIM is lower because the embedded face is WenQuanYi Micro Hei, not the source font. On the AutoCompact page, 2 of 11 protected tokens were not recovered after the identity rewrite. Reference sections are not on these first pages, so `reference_byte_identity` is 1.0 with an empty operator set. The unit test still checks that a synthetic bibliography stays byte-identical.

`arxiv-2609.15979` (The k-server conjecture is true, CC BY 4.0) was added to the manifest. The PDF is only in the gitignored cache.

## Identity ceiling

Recorded 2026-10-05T00:06:35Z on the tree that added Type 1 encoding and `rpt-bench` (parent `fdfa03f`).

This snapshot scores each CI PDF against itself. That is the ceiling for drop rate, style, overflow, and non-text SSIM, and it is the live measurement of source-text recovery (`source_unmapped_ratio`). RapidPdfTrans does not emit a translated PDF yet, so it is not on the output comparison.

## Means

- Documents scored: 9
- Source glyphs: 369426
- Source unmapped: 483 (0.0013)
- Mean line coverage: 1.0000
- Mean drop rate: 0.0000
- Mean protected recall: 1.0000
- Mean formula integrity: 1.0000
- Mean overflow rate: 0.0000
- Mean style retention: 1.0000
- Mean identity character retention: 1.0000
- Mean non-text SSIM: 1.0000

The previous fidelity report, before Type 1 built-in encodings were read, had 1,369 unmapped glyphs on these same nine files. On the full 89-document cache the same change cut unmapped glyphs from 45,140 to 16,427, and the fraction of Poppler characters found in our extraction rose from 0.9797 to 0.9865. Identity text, style, and SSIM stayed clean (89/89, mean SSIM 1.0000).

## Engines

- `rapidpdftrans`: identity rewrite of page 1 works (see the rewrite round above). A live model was not called.
- `babeldoc`: not_installed
- `pdf2zh`: not_installed
- `pdf2zh_next`: not_installed
- `llm-judge`: skipped_no_key (Reference metric for an identity translator is identity_char_retention. A live judge is not called from this script.)

## Priority

1. The identity rewrite drops no lines, and the remaining page-SSIM gap is the substituted font. Two protected tokens on `arxiv-2610.02163` page 1 still need a cause. A live translation is blocked until `RPT_LLM_API_KEY` is available.
2. The embedded face is one CJK font, so page SSIM falls even when the characters match. Reusing the source font's advances, and OpenType GSUB, are still open. The older CMEX name gap is mapped.
3. BabelDOC and PDFMathTranslate are not installed in this environment, so this snapshot has no external output scores. `corpus/bench/run.py --run-engines` is the hook once the commands exist and a shared translator URL is set.

## Per file

| id | glyphs | unmapped | drop | protected | formula | overflow | style | non-text SSIM |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `arxiv-2610.02163` | 35548 | 0 | 0.0000 | 1.0000 | 1.0000 | 0.0000 | 1.0000 | 1.0000 |
| `pmlr-v202-abbas23a` | 39351 | 3 | 0.0000 | 1.0000 | 1.0000 | 0.0000 | 1.0000 | 1.0000 |
| `neurips-2023-00296c0e` | 33417 | 38 | 0.0000 | 1.0000 | 1.0000 | 0.0000 | 1.0000 | 1.0000 |
| `arxiv-2610.01998` | 19126 | 152 | 0.0000 | 1.0000 | 1.0000 | 0.0000 | 1.0000 | 1.0000 |
| `pmlr-v202-abels23a` | 44038 | 7 | 0.0000 | 1.0000 | 1.0000 | 0.0000 | 1.0000 | 1.0000 |
| `neurips-2017-attention` | 27708 | 4 | 0.0000 | 1.0000 | 1.0000 | 0.0000 | 1.0000 | 1.0000 |
| `2024-acl-long-7` | 45945 | 28 | 0.0000 | 1.0000 | 1.0000 | 0.0000 | 1.0000 | 1.0000 |
| `arxiv-2610.02193` | 66492 | 227 | 0.0000 | 1.0000 | 1.0000 | 0.0000 | 1.0000 | 1.0000 |
| `arxiv-2609.36965` | 57801 | 24 | 0.0000 | 1.0000 | 1.0000 | 0.0000 | 1.0000 | 1.0000 |
