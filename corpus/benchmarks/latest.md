# Translation benchmark

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

- `rapidpdftrans`: no_translated_pdf (rpt_save still returns 'PDF rewriting is not implemented (milestone M3)'. Output metrics are not available.)
- `babeldoc`: not_installed
- `pdf2zh`: not_installed
- `pdf2zh_next`: not_installed
- `llm-judge`: skipped_no_key (Reference metric for an identity translator is identity_char_retention. A live judge is not called from this script.)

## Priority

1. Translated output does not exist yet (`rpt_save` is still the M3 stub). Every layout comparison against BabelDOC is blocked on a rewrite that deletes original text operators without dropping glyphs.
2. The glyphs that stay unmapped are almost all CMEX (big operators and delimiters). Their Type 1 names are not in the Adobe Glyph List, so formula characters still never reach the translator.
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
