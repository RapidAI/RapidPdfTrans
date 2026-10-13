# arXiv layout check

Four English arXiv papers were added to `corpus/manifest.json` (URL, license, sha256; the PDFs stay in gitignored `corpus/cache/`). Each was translated in full to Chinese on current main. Comparison used glyph coordinates from `rpt extract` as the layout evidence, with `pdftotext -layout` and side-by-side page renders as a second view. Figure interiors, table cells, and bibliographies staying in English is the intended default.

Translated PDFs (not committed). The same files are copied to `/opt/cursor/artifacts/pdfs/`.

| id | pages | PDF |
| --- | --- | --- |
| arxiv-1908.07836 PubLayNet | 8, two-column | `/opt/cursor/artifacts/pdfs/arxiv-1908.07836-zh.pdf` |
| arxiv-2204.08387 LayoutLMv3 | 10, two-column | `/opt/cursor/artifacts/pdfs/arxiv-2204.08387-zh.pdf` |
| arxiv-2305.03393 OTSL table tokenization | 14, single-column | `/opt/cursor/artifacts/pdfs/arxiv-2305.03393-zh.pdf` |
| arxiv-2512.09874 formula-parser benchmark | 16, single-column, math | `/opt/cursor/artifacts/pdfs/arxiv-2512.09874-zh.pdf` |

All four reports have `coverage.complete: true`. Chinese body is `RPTCJK` CID Type 0C Identity-H. Source Latin faces (Nimbus, Computer Modern) remain for text that is kept.

Side-by-side renders (source left, translation right):

- `/opt/cursor/artifacts/arxiv-layout/arxiv-1908.07836-p01.png`
- `/opt/cursor/artifacts/arxiv-layout/arxiv-1908.07836-p03.png`
- `/opt/cursor/artifacts/arxiv-layout/arxiv-2204.08387-p01.png`
- `/opt/cursor/artifacts/arxiv-layout/arxiv-2204.08387-p04.png`
- `/opt/cursor/artifacts/arxiv-layout/arxiv-2305.03393-p03.png`
- `/opt/cursor/artifacts/arxiv-layout/arxiv-2305.03393-p09.png`
- `/opt/cursor/artifacts/arxiv-layout/arxiv-2305.03393-p10.png`
- `/opt/cursor/artifacts/arxiv-layout/arxiv-2512.09874-p01.png`

## Fixes applied on main while reviewing

| commit | defect | where it showed |
| --- | --- | --- |
| `67ed4b7` | A two-column gutter of about 1.2em was one segment. | PubLayNet abstract |
| `0859ceb` | Table-header cells on one baseline merged with the other column. | PubLayNet page 2 header |
| `c47dcde` | Decimals, relations, and version tokens rose as author marks. One missing math letter dropped the paragraph. Roman table captions did not keep cells. A right-column bibliography swallowed the other column. An address wrapped onto the email. | PubLayNet pages 1, 4–5, 7; LayoutLMv3 |
| `af58a31` | A short note in the other column (`where α is 32.`) sat just under the 24pt hard gap, so the paragraph stayed English. | LayoutLMv3 page 4, Objective II |
| `72699e9` | Small figure labels outnumbered the body on a shared baseline, so the line size became ~2.6pt and every word space was a column split. HTML table markup was treated as an untranslated paragraph. | PubLayNet page 3; OTSL HTML figure |
| `06bd856` | A footnote number plus a URL failed coverage when left unchanged. | Formula paper page 4 |
| `8a99948` | ORCID ids `[0000-0002-…]` matched the `[1]` bibliography rule and kept the introduction in English until the next heading. | OTSL pages 2–3 |
| `7fbb5b1` | A margin stamp widened the baseline window, so two abstract lines were collected as one interleaved sentence. A caption label’s word space was treated as a column gap, and a dotted heading such as `5.3` did not stop the table walk. | Formula paper page 1; OTSL pages 9–10 |
| `ab2e6c2` | A table left the page transform shifted, so text appended after it painted on top of the cells. | OTSL page 10 |

## arxiv-1908.07836 PubLayNet (two-column)

Letter, 612×792. Journal figures on pages 2–3 and the bibliography stay English on purpose.

| page | findings | severity | fixed |
| --- | --- | --- | --- |
| 1 | Title, abstract, and introduction are Chinese in the source columns. Author names and emails stay Latin. The vertical arXiv stamp stays original. Emails are on their own baselines and no longer collide with the address line. | low (names and stamp are kept by design) | address/email collision fixed in `c47dcde` |
| 2 | Reproduced journal page and table legend stay English inside the figure. Caption is Chinese (`图`). The header row `Document layout` / `XML category` is no longer one overflow segment. | — | header split fixed in `0859ceb` |
| 3 | Body is one Chinese paragraph again (`通过 PDFMiner 解析的 PDF 内容与 XML 节点的文本之间存在频繁的小差异…莱文斯坦距离 [18]`). Previously each word was its own segment and Chinese was painted on top of the English. The journal figure on the right stays English. | was high | fixed in `72699e9` |
| 4 | Body columns are Chinese. Table II cells stay source Latin and source positions (no stacked `0.902`). | was high | roman captions in `c47dcde` |
| 5 | Same as page 4 for Tables III and IV. The learning-rate `0.001` is no longer a raised `0`. | was high | decimal guard in `c47dcde` |
| 6 | Discussion is Chinese. Figure category labels (Text, Title, Table, Figure, List) stay English. | — | — |
| 7 | Left-column conclusion is Chinese. Right-column references `[1]`–`[18]` stay English. Table V cells stay English. | was high | reference-column guard in `c47dcde` |
| 8 | Rest of the bibliography, English. | — | — |

## arxiv-2204.08387 LayoutLMv3 (two-column)

| page | findings | severity | fixed |
| --- | --- | --- | --- |
| 1 | Title and abstract are Chinese. Author names stay Latin. Emails stay emails. A raised star on an author is a real affiliation mark. | low | math-letter fallback in `c47dcde` stopped missing `𝑀`/`𝜆` from dropping paragraphs |
| 2 | Body is Chinese. Figure token labels (Masked Word, `[CLS]`, `[MASK]`) stay English. | — | — |
| 3 | Diagram of MLM / WPA / MIM heads stays English inside the figure. Surrounding prose is Chinese. | — | — |
| 4 | Objective II is Chinese (`目标 II：掩码图像建模（MIM）…`), including the note that used to sit past a 23.6pt gutter (`where α is 32.`). Equation (1) stays math. | was high (coverage failed, 442 glyphs) | column stub in `af58a31` |
| 5 | Body Chinese. Display math stays Computer Modern. | — | — |
| 6 | Body Chinese. | — | — |
| 7 | Prose is Chinese. One results-table band still has English header words (`Image`, `Pre-training`, `FUNSD`) because those cells are kept. A short English fragment (`models of LayoutLMv3 on PubLayNet dataset`) remains in that band. | medium | not fixed; cells are kept by design, the fragment is a leftover label in the same band |
| 8 | Body Chinese. | — | — |
| 9 | Appendix A is partly Chinese. The reference list that shares the page stays English. | low | — |
| 10 | Bibliography only, English. | — | — |

## arxiv-2305.03393 OTSL (single-column, tables)

| page | findings | severity | fixed |
| --- | --- | --- | --- |
| 1 | Title, abstract, and the start of the introduction are Chinese. Author names, ORCID ids, and the email stay Latin. | low | ORCID no longer opens a bibliography (`8a99948`) |
| 2 | Introduction under Figure 1 is Chinese. Figure 1’s HTML/OTSL drawing and its in-figure vocabulary labels stay English. The caption is still the English source line `Fig. 1. Comparison…` (kept with the figure). | medium (caption) | introduction fixed in `8a99948`; caption not fixed |
| 3 | The rest of the introduction and Related Work are Chinese. No long English body lines. | was high | fixed in `8a99948` |
| 4 | Section prose is Chinese. | — | — |
| 5 | Section prose is Chinese. | — | — |
| 6 | Section prose is Chinese. | — | — |
| 7 | Section prose is Chinese. | — | — |
| 8 | Section prose is Chinese. OTSL token rows that are markup stay as source tokens. | — | markup no longer fails coverage (`72699e9`) |
| 9 | The paragraph above Table 1 is Chinese. The full caption is Chinese (`表 1. 在相同基于 transformer 的 TableFormer…` through the smaller-model sentence). Cells and headers (`OTSL`, `HTML`, `TEDs`, the scores) stay English on the source baselines. | was medium | caption join fixed in `7fbb5b1` |
| 10 | Table 2’s caption is Chinese; its cells stay English. `5.3 定性结果` and the paragraph under it (`为了说明 OTSL 和 HTML 之间的定性差异…`) are Chinese and sit on the source baselines (x=134.8), under the table. Figure 5’s drawing labels stay English. | was medium | heading fixed in `7fbb5b1`; the leaked page transform that had shifted this text onto the cells is fixed in `ab2e6c2` |
| 11 | Figure continuation, English labels inside the figure. | — | — |
| 12 | Conclusion prose is Chinese. References begin and stay English. | — | — |
| 13 | Bibliography, English. | — | — |
| 14 | Bibliography, English. | — | — |

## arxiv-2512.09874 formula benchmark (single-column, math)

| page | findings | severity | fixed |
| --- | --- | --- | --- |
| 1 | Title, authors, affiliations, the abstract, and the keywords are Chinese. Author marks `Horn1` and `Keuper1,2` stay superscripts. The abstract is one paragraph on the source block: it starts on the Abstract baseline and the keywords sit on the Keywords baseline. The old interleaved `ipnigpe(lCinDeMco)…` is gone; `(CDM)` and `pipeline combining` are consecutive lines again. | was high | fixed in `7fbb5b1` |
| 2 | Body Chinese. Inline math stays Computer Modern. | — | — |
| 3 | Body Chinese. | — | — |
| 4 | Body Chinese. The dataset URL stays a URL and no longer fails coverage. | was high (coverage, 57 glyphs) | `06bd856` |
| 5 | Short page, Chinese. | — | — |
| 6 | Body Chinese. Display formulas stay math fonts. | — | — |
| 7 | Body Chinese. The metric-study URL stays a URL. | — | — |
| 8 | Body Chinese. | — | — |
| 9 | Scatter-plot page. Axis titles and legend text (`BLEU Score`, `Human Score`, `CDM Score`) stay English inside the figure. | — | — |
| 10 | Body Chinese. Model names stay Latin. | — | — |
| 11 | Body Chinese. Parser names stay Latin. | — | — |
| 12 | Results table: caption area is partly Chinese; header words (`Rank`, `Parser`, `Score`) stay with the table. | low | — |
| 13 | Bibliography starts, English. | — | — |
| 14 | Bibliography, English. | — | — |
| 15 | Bibliography, English. URLs stay URLs. | — | — |
| 16 | Last reference lines, English. | — | — |

## Still open

1. **LayoutLMv3 page 7 (medium).** A short English fragment remains in the results-table band next to kept header cells.
2. **OTSL page 2 (medium).** The Figure 1 caption (`Fig. 1. Comparison…`) stays the English source line. The drawing and its in-figure labels stay English by design.
