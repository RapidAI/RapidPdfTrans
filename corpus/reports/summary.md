# Corpus fidelity report

Identity checks copy the original PDF bytes and compare extraction, per-glyph style, and rendered pages. The lopdf rewrite is a separate structural round-trip. It is not translated output. PDFium is not installed; text cross-check uses Poppler `pdftotext` when present. Extraction leaves every glyph `pending`, so `coverage_complete` stays false until a later milestone rewrites or explicitly keeps each glyph. That is the conservation baseline, not a failed extract.

- Documents: 89
- Extracted: 89
- Open failures: 0
- Panics: 0
- Glyphs: 4269057
- Unmapped glyphs: 45140
- Page-capped documents: 10
- Identity text matches: 89
- Identity style/position clean: 89
- lopdf rewrite text matches: 87 of 87 attempted
- lopdf rewrite failures: 0
- lopdf rewrite skipped above 25MB: 2
- Mean fraction of our characters found by Poppler: 0.9939
- Mean fraction of Poppler characters found by us: 0.9797
- Mean identity-page SSIM: 1.0000

## By source

- arxiv: 33
- pmlr: 10
- acl: 8
- neurips: 7
- cvpr: 6
- emnlp: 6
- usenix: 5
- iccv: 4
- naacl: 4
- ctan: 1
- eccv: 1
- nist: 1
- open-logic: 1
- openstax: 1
- wikimedia: 1

## Manifest tags

- latex: 86
- two-column: 58
- conference: 51
- arxiv: 33
- ml: 24
- nlp: 23
- figures: 19
- cv: 16
- cs: 15
- formulas: 11
- icml: 10
- pmlr: 10
- acl: 8
- security: 8
- neurips: 7
- cvpr: 6
- emnlp: 6
- cjk: 5
- code: 5
- usenix: 5
- biology: 4
- book: 4
- iccv: 4
- math: 4
- naacl: 4
- physics: 4
- tables: 3
- public-domain: 2
- single-column: 2
- stats: 2
- textbook: 2
- algorithms: 1
- eccv: 1
- government: 1
- historical: 1
- literature: 1
- manual: 1
- prince: 1
- scanned: 1
- word: 1

## Observed feature tags

These come from the extracted glyphs (column gutters, font names, Unicode ranges), not from the manifest.

- color: 82
- formulas: 81
- italic: 80
- bold: 72
- figures: 72
- footnotes: 62
- tables: 62
- two-column: 57
- single-column: 32
- code: 26
- cjk: 7

## Per file

| id | status | pages | glyphs | unmapped | poppler | identity SSIM | lopdf text |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `2024-acl-long-0` | ok | 95 | 16214 | 0 | 0.999 | 1.0000 | match |
| `2024-acl-long-1` | ok | 17 | 60735 | 4 | 0.996 | 1.0000 | match |
| `2024-acl-long-2` | ok | 18 | 61452 | 52 | 0.994 | 1.0000 | match |
| `2024-acl-long-3` | ok | 18 | 59888 | 0 | 0.995 | 1.0000 | match |
| `2024-acl-long-4` | ok | 20 | 64572 | 7 | 0.995 | 1.0000 | match |
| `2024-acl-long-5` | ok | 17 | 58052 | 1 | 0.996 | 1.0000 | match |
| `2024-acl-long-6` | ok | 11 | 38116 | 3 | 0.995 | 1.0000 | match |
| `2024-acl-long-7` | ok | 15 | 45945 | 28 | 0.995 | 1.0000 | match |
| `arxiv-1312.6114` | ok | 14 | 34820 | 3767 | 0.997 | 1.0000 | match |
| `arxiv-1406.2661` | ok | 9 | 24451 | 1196 | 0.996 | 1.0000 | match |
| `arxiv-1409.1556` | ok | 14 | 45981 | 6 | 0.994 | 1.0000 | match |
| `arxiv-1412.6980` | ok | 15 | 34811 | 350 | 0.997 | 1.0000 | match |
| `arxiv-1502.03167` | ok | 11 | 38051 | 51 | 0.993 | 1.0000 | match |
| `arxiv-1506.02640` | ok | 10 | 35243 | 389 | 0.994 | 1.0000 | match |
| `arxiv-1512.03385` | ok | 12 | 49830 | 469 | 0.994 | 1.0000 | match |
| `arxiv-1607.06450` | ok | 14 | 38815 | 2283 | 0.998 | 1.0000 | match |
| `arxiv-1703.06870` | ok | 12 | 52222 | 258 | 0.994 | 1.0000 | match |
| `arxiv-1706.03762` | ok | 15 | 33460 | 1 | 1.000 | 1.0000 | match |
| `arxiv-1810.04805` | ok | 16 | 54003 | 223 | 0.990 | 1.0000 | match |
| `arxiv-2006.11239` | ok | 25 | 47963 | 3447 | 0.997 | 1.0000 | match |
| `arxiv-2010.11929` | ok | 22 | 56730 | 1021 | 0.996 | 1.0000 | match |
| `arxiv-2103.14030` | ok | 14 | 57309 | 638 | 0.992 | 1.0000 | match |
| `arxiv-2609.32770` | ok | 35 | 98159 | 1 | 0.998 | 1.0000 | match |
| `arxiv-2609.33014` | ok | 16 | 33847 | 1 | 1.000 | 1.0000 | match |
| `arxiv-2609.36804` | ok | 16 | 50103 | 24 | 0.995 | 1.0000 | match |
| `arxiv-2609.36965` | ok | 20 | 57801 | 24 | 1.000 | 1.0000 | match |
| `arxiv-2609.38908` | ok | 31 | 93280 | 55 | 0.999 | 1.0000 | match |
| `arxiv-2610.01358` | ok | 47 | 35869 | 29 | 0.999 | 1.0000 | match |
| `arxiv-2610.01706` | ok | 9 | 29350 | 14 | 0.995 | 1.0000 | match |
| `arxiv-2610.01891` | ok | 20 | 58977 | 17 | 0.999 | 1.0000 | match |
| `arxiv-2610.01899` | ok | 18 | 33380 | 147 | 1.000 | 1.0000 | match |
| `arxiv-2610.01980` | ok | 14 | 32224 | 194 | 0.999 | 1.0000 | match |
| `arxiv-2610.01995` | ok | 38 | 85764 | 781 | 1.000 | 1.0000 | match |
| `arxiv-2610.01998` | ok | 10 | 19126 | 152 | 0.999 | 1.0000 | match |
| `arxiv-2610.02080` | ok | 14 | 19332 | 0 | 0.999 | 1.0000 | match |
| `arxiv-2610.02109` | ok | 31 | 59880 | 268 | 0.999 | 1.0000 | match |
| `arxiv-2610.02113` | ok | 22 | 84876 | 168 | 0.999 | 1.0000 | match |
| `arxiv-2610.02158` | ok | 26 | 60992 | 398 | 0.999 | 1.0000 | match |
| `arxiv-2610.02163` | ok | 11 | 35548 | 0 | 0.998 | 1.0000 | match |
| `arxiv-2610.02193` | ok | 24 | 66492 | 227 | 0.999 | 1.0000 | match |
| `arxiv-2610.02209` | ok | 27 | 47871 | 222 | 0.998 | 1.0000 | match |
| `ctex-manual` | ok | 195 | 14912 | 15 | 1.000 | 1.0000 | match |
| `cvpr-2024-carlsson-heal-swin-a-vision-transformer-on-the-s` | ok | 11 | 46379 | 0 | 0.995 | 1.0000 | match |
| `cvpr-2024-decatur-3d-paintbrush-local-stylization-of-3d-sh` | ok | 11 | 40498 | 1 | 0.994 | 1.0000 | match |
| `cvpr-2024-lee-guided-slot-attention-for-unsupervised-video` | ok | 10 | 37232 | 157 | 0.994 | 1.0000 | match |
| `cvpr-2024-liu-programmable-motion-generation-for-open-set-` | ok | 10 | 43263 | 2 | 0.996 | 1.0000 | match |
| `cvpr-2024-yin-sce-mae-selective-correspondence-enhancement` | ok | 10 | 41484 | 5 | 0.994 | 1.0000 | match |
| `cvpr-2024-zhu-dpmesh-exploiting-diffusion-prior-for-occlud` | ok | 10 | 41163 | 98 | 0.994 | 1.0000 | match |
| `eccv-2022-001` | ok | 20 | 44344 | 113 | 0.997 | 1.0000 | match |
| `2023-emnlp-main-0` | ok | 101 | 16389 | 0 | 0.999 | 1.0000 | match |
| `2023-emnlp-main-1` | ok | 14 | 45111 | 6 | 0.994 | 1.0000 | match |
| `2023-emnlp-main-2` | ok | 14 | 38729 | 0 | 0.995 | 1.0000 | match |
| `2023-emnlp-main-3` | ok | 14 | 50177 | 0 | 0.995 | 1.0000 | match |
| `2023-emnlp-main-4` | ok | 15 | 48082 | 0 | 0.995 | 1.0000 | match |
| `2023-emnlp-main-5` | ok | 13 | 35181 | 14 | 0.996 | 1.0000 | match |
| `iccv-2023-han-towards-attack-tolerant-federated-learning-v` | ok | 10 | 38129 | 12 | 0.997 | 1.0000 | match |
| `iccv-2023-liu-birds-eye-view-scene-graph-for-vision-langua` | ok | 13 | 53992 | 119 | 0.984 | 1.0000 | match |
| `iccv-2023-yi-diff-retinex-rethinking-low-light-image-enhan` | ok | 10 | 36859 | 1072 | 0.992 | 1.0000 | match |
| `iccv-2023-zbinden-stochastic-segmentation-with-conditional` | ok | 11 | 41263 | 33 | 0.990 | 1.0000 | match |
| `2024-naacl-long-0` | ok | 62 | 14399 | 0 | 0.999 | 1.0000 | match |
| `2024-naacl-long-1` | ok | 21 | 74440 | 0 | 0.996 | 1.0000 | match |
| `2024-naacl-long-2` | ok | 18 | 51532 | 74 | 0.992 | 1.0000 | match |
| `2024-naacl-long-3` | ok | 19 | 69599 | 0 | 0.996 | 1.0000 | match |
| `neurips-2017-attention` | ok | 11 | 27708 | 890 | 0.998 | 1.0000 | match |
| `neurips-2023-0001ca33` | ok | 12 | 34157 | 0 | 0.996 | 1.0000 | match |
| `neurips-2023-00160816` | ok | 22 | 65052 | 14 | 0.999 | 1.0000 | match |
| `neurips-2023-0021c2cb` | ok | 22 | 51479 | 24 | 0.999 | 1.0000 | match |
| `neurips-2023-00226294` | ok | 16 | 57578 | 0 | 0.940 | 1.0000 | match |
| `neurips-2023-00296c0e` | ok | 11 | 33417 | 38 | 0.999 | 1.0000 | match |
| `neurips-2023-0073cc73` | ok | 13 | 40714 | 9 | 0.989 | 1.0000 | match |
| `nist-sp-800-63-3` | ok | 76 | 19283 | 7251 | 0.852 | 1.0000 | match |
| `open-logic` | ok | 1016 | 19421 | 0 | 0.999 | 1.0000 | match |
| `openstax-university-physics-v2` | ok | 781 | 8613 | 1359 | 0.997 | 1.0000 | skip |
| `pmlr-v202-aamand23a` | ok | 18 | 51769 | 129 | 0.995 | 1.0000 | match |
| `pmlr-v202-abbas23a` | ok | 12 | 39351 | 3 | 0.997 | 1.0000 | match |
| `pmlr-v202-abbe23a` | ok | 30 | 90692 | 252 | 0.997 | 1.0000 | match |
| `pmlr-v202-abedsoltan23a` | ok | 18 | 47727 | 101 | 0.998 | 1.0000 | match |
| `pmlr-v202-abels23a` | ok | 12 | 44038 | 7 | 0.997 | 1.0000 | match |
| `pmlr-v202-acharki23a` | ok | 42 | 45785 | 125 | 0.996 | 1.0000 | match |
| `pmlr-v202-adams23a` | ok | 19 | 54467 | 310 | 0.991 | 1.0000 | match |
| `pmlr-v202-agarwala23a` | ok | 17 | 47590 | 4348 | 0.996 | 1.0000 | match |
| `pmlr-v202-agarwala23b` | ok | 27 | 64412 | 10133 | 0.996 | 1.0000 | match |
| `pmlr-v202-agazzi23a` | ok | 32 | 74309 | 646 | 0.996 | 1.0000 | match |
| `usenix-sec24-gohil` | ok | 19 | 71498 | 293 | 0.996 | 1.0000 | match |
| `usenix-sec24-mankali` | ok | 19 | 84574 | 97 | 0.995 | 1.0000 | match |
| `usenix-sec24-qin` | ok | 19 | 90453 | 195 | 0.997 | 1.0000 | match |
| `usenix-sec24-soneji` | ok | 19 | 83513 | 107 | 0.996 | 1.0000 | match |
| `usenix-sec24-tabassum` | ok | 19 | 85829 | 172 | 0.996 | 1.0000 | match |
| `lang-blue-fairy-1889` | ok | 438 | 937 | 0 | 1.000 | 1.0000 | skip |

## Unmapped glyphs

A glyph is kept and flagged when no Unicode mapping is found. The large clusters are older pdfTeX files: Computer Modern Type 1 subsets with Builtin or Custom encodings and no ToUnicode CMap, so OT1/OML/OMS codes stay unmapped. The Word-produced NIST file is the other cluster (subsetted Times and Arial). Poppler still recovers most of those characters.

- `arxiv-1312.6114`: 3767 / 34820 (10.8%) — PTTSAO+CMR10 (1160), ANCPWH+CMMI10 (477), UVUOJU+CMBX10 (464), RSAPAN+CMR7 (431)
- `arxiv-1406.2661`: 1196 / 24451 (4.9%) — KZINBN+CMR10 (442), EINEDN+CMMI10 (299), FKSCQX+CMMI7 (93), ZYPRQG+CMMIB10 (64)
- `arxiv-1607.06450`: 2283 / 38815 (5.9%) — WYHSSF+CMR10 (476), MJRMXB+CMMI10 (453), TSLWER+CMMI7 (372), OTIYPV+CMTT9 (235)
- `arxiv-2006.11239`: 3447 / 47963 (7.2%) — VFFYMT+CMR10 (800), NLZWEU+CMMI7 (454), CQJYMI+CMR7 (450), XICDVQ+CMMI10 (408)
- `iccv-2023-yi-diff-retinex-rethinking-low-light-image-enhan`: 1072 / 36859 (2.9%) — NAMWUE+CMMI10 (352), NSCXEH+CMMI7 (298), OBIDOX+CMR10 (204), DJTDZM+CMSY10 (99)
- `neurips-2017-attention`: 890 / 27708 (3.2%) — FUIULY+CMR10 (318), LICAEO+CMMI10 (303), EDCQSD+CMMI7 (98), JQKXPN+CMR7 (66)
- `nist-sp-800-63-3`: 7251 / 19283 (37.6%) — CVGOYE+TimesNewRomanPSMT (3628), WFZUSQ+ArialMT (2616), XWQAGO+Arial-BoldMT (565), RPTPHP+Calibri (165)
- `openstax-university-physics-v2`: 1359 / 8613 (15.8%) — AAAAAK+NotoSans-Regular (1056), OIWKUU+HelveticaNeue-Bold (84), AAAAAJ+NotoSans-Bold (77), OIWKUU+HelveticaNeue (74)
- `pmlr-v202-agarwala23a`: 4348 / 47590 (9.1%) — LRYYAT+CMR10 (1055), UVOIKI+CMMI10 (732), EUXJPC+CMMI7 (614), TYXLXL+CMBX10 (529)
- `pmlr-v202-agarwala23b`: 10133 / 64412 (15.7%) — BBTCWP+CMR10 (3235), XTWTKF+CMMI10 (2552), TMACHH+CMMI7 (1547), OQZUZG+CMR7 (913)

## Diagnostics

- `openstax-university-physics-v2`: 1 — page has no content stream
- `usenix-sec24-tabassum`: 10 — trailing operands at byte 1595; trailing operands at byte 1611; operator 'TJ' skipped: operand 0 is not an array; operator 'TJ' skipped: operand 0 is not an array; trailing operands at byte 1620

Per-page scores and the full font lists are in the JSON report.
