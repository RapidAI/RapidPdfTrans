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
| `b10b557` | A caption wrap was dropped when the other column sat between it and the first caption line. | LayoutLMv3 page 7, Figure 4 |

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
| 7 | Prose is Chinese. Table 3 cells stay English (`Image`, `Pre-training`, `FUNSD`, the scores). Figure 4’s caption is the full Chinese sentence, including `LayoutLMv3 各消融模型在 PubLayNet 数据集上微调的损失收敛曲线` through `建议彩色查看`. The chart interior stays English. | was medium | caption wrap fixed in `b10b557` and the page retranslated |
| 8 | Body Chinese. | — | — |
| 9 | Appendix A is partly Chinese. The reference list that shares the page stays English. | low | — |
| 10 | Bibliography only, English. | — | — |

## arxiv-2305.03393 OTSL (single-column, tables)

| page | findings | severity | fixed |
| --- | --- | --- | --- |
| 1 | Title, abstract, and the start of the introduction are Chinese. Author names, ORCID ids, and the email stay Latin. | low | ORCID no longer opens a bibliography (`8a99948`) |
| 2 | Introduction under Figure 1 is Chinese. Figure 1’s HTML/OTSL drawing and its in-figure vocabulary labels stay English. The caption is Chinese (`图 1. HTML 和 OTSL 表格结构表示之间的比较` through the fixed row length). | was medium (caption) | caption wrap fixed in `7fbb5b1`; drawing stays English by design |
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

## Closed

LayoutLMv3 page 7 and the OTSL Figure 1 caption are Chinese. Table cells and the HTML/OTSL drawing stay English by design.

Side-by-side of the refreshed LayoutLMv3 page: `/opt/cursor/artifacts/arxiv-layout/arxiv-2204.08387-p07.png`.

## Next three papers

These three were already in `corpus/manifest.json`. Each was translated in full. Figure interiors, table cells, display equations, and bibliographies stay English.

| id | pages | PDF |
| --- | --- | --- |
| arxiv-2606.23344 RT-DocLayout | 16, two-column, A4 | `/opt/cursor/artifacts/pdfs/arxiv-2606.23344-zh.pdf` |
| arxiv-2607.01018 reading-order inference | 17, single-column | `/opt/cursor/artifacts/pdfs/arxiv-2607.01018-zh.pdf` |
| arxiv-2608.06146 PaDoc | 17, two-column | `/opt/cursor/artifacts/pdfs/arxiv-2608.06146-zh.pdf` |

All three reports have `coverage.complete: true` and `english_body: 0`.

Side-by-side renders:

- `/opt/cursor/artifacts/arxiv-layout/arxiv-2606.23344-p05.png`
- `/opt/cursor/artifacts/arxiv-layout/arxiv-2606.23344-p06.png`
- `/opt/cursor/artifacts/arxiv-layout/arxiv-2607.01018-p02.png`
- `/opt/cursor/artifacts/arxiv-layout/arxiv-2607.01018-p06.png`
- `/opt/cursor/artifacts/arxiv-layout/arxiv-2608.06146-p02.png`
- `/opt/cursor/artifacts/arxiv-layout/arxiv-2608.06146-p10.png`

### Fixes in this change

| defect | where it showed |
| --- | --- |
| `Figure 2 \| …` was not a caption, and the centered wrap `and reading order prediction.` was glued onto the loss paragraph. | RT-DocLayout page 5 |
| A display equation counted as the other column, so `the order loss` did not continue as `operates on` on the next page. | RT-DocLayout pages 5–6 |
| A hyphen with body text still under it was treated as the end of the page, so `embedding space` joined the next page. | RT-DocLayout |
| `Overall Objective.` and `Mask Segmentation Loss.` were restored as personal names. A run-in heading that ends in a period after a full word stays translated. `Jr.` still counts as a name. | RT-DocLayout |
| `Prefix-Conditioned Sufficiency` was restored as a byline. A hyphenated compound with two long parts is a title. `Jean-Luc Picard` stays a name. | PaDoc appendix A |
| `A Derivations` and `A.1 From Region-Specific…` did not end the bibliography, so appendices A–E stayed English. | PaDoc |
| `1 Introduction` in CMBX under the caption was a figure label. A bold Computer Modern title is a section heading. A semibold diagram label is not. | Reading-order paper page 2 |
| One missing `ℎ` (U+210E), `×`, or `∼` dropped the whole paragraph. Those stand in for `h`, `x`, and `~`. | RT-DocLayout embedding sentence; PaDoc throughput sentence |
| `N×N` a few points above the words became its own baseline, so `得到` was painted on top of the next line. A short math-script cluster joins the line it sits on. | RT-DocLayout page 5 |
| `Applying this with A = Y_k` and `Since the conditioning…` share a baseline with subscripts and were kept as formulas. A sentence with a function word still translates; `Lbbox` stays an equation. | PaDoc appendix A |

## arxiv-2606.23344 RT-DocLayout (two-column)

| page | findings | severity | fixed |
| --- | --- | --- | --- |
| 5 | Figure 2’s caption is the full Chinese sentence, including the centered wrap about reading-order prediction. The formula lead-in `where Wq, Wk` stays math. The following prose is Chinese. The embedding sentence is Chinese; `dh` stands in for the Planck constant in `𝑑ℎ`. `由此得到的关系矩阵` is one line. | was high | caption bar, caption wrap, page-floor hyphen, Planck stand-in, script baseline |
| 6 | `顺序损失` on page 5 continues as `作用于 O(N^2)` on this page. Run-in headings `总体目标。` and `掩码分割损失。` are Chinese. | was high | equation no longer blocks the page break; headings are not bylines |
| 14–16 | Bibliography, English. Page 13 is still body (`4.4 定性分析`). | — | — |

## arxiv-2607.01018 reading order (single-column)

| page | findings | severity | fixed |
| --- | --- | --- | --- |
| 1 | Title, abstract, and keywords are Chinese. Author names and emails stay Latin. | low | — |
| 2 | The section heading is `1 引言`. Body columns are Chinese. Figure 1’s caption is Chinese. The diagram interior stays English. | was medium | CMBX heading is not a figure label |
| 6 | Run-in headings `上下文截断.` and `可选频率归一化.` are Chinese. Display formulas stay math. | was medium | heading period is not a byline |
| 17 | Bibliography, English. Pages 15–16 are still body. | — | — |

## arxiv-2608.06146 PaDoc (two-column)

| page | findings | severity | fixed |
| --- | --- | --- | --- |
| 2 | Body is Chinese, including the OmniDocBench / throughput paragraph (`吞吐量提高了67.4–118%`). Figure labels (`ViT`, `Layout Analysis Model`, `<BOS>`) stay English. | was high (534 glyphs) | `∼` stand-in |
| 8–9 | Bibliography, English. AAAI puts it before the appendix. | — | — |
| 10 | Appendix A is Chinese (`A 推导`, `A.1 从区域特定充分性到`, `前缀条件充分性`). The subscript lead-ins are prose: `将其应用于 A = Yk…`, `由于条件 (X,Bk,B<k) 作为元组是相同的`, and `到 (X,B≤k)，这变为`. Display equations stay math. | was low | lettered appendix; compound title; subscript sentences |
| 11–17 | Appendices B–E are Chinese. Table cells and display formulas stay English. | was high | bibliography stops at `A Derivations` |

## Textbook, slides, and a Word report

Three openly licensed PDFs. Each translation is coverage-complete (`english_body` 0). Figure interiors, table cells, display equations, and bibliographies stay English. The full Manning book was not translated. A replay of pages 1–30 matches the v32 contents coordinates.

| id | pages | PDF |
| --- | --- | --- |
| ocw-18-225-lec01, MIT OCW 18.225 lecture 1, CC BY-NC-SA | 11, letter | `/opt/cursor/artifacts/pdfs/ocw-18-225-lec01-zh.pdf` |
| fosdem-2024-mutation, CC BY | 61, landscape slides | `/opt/cursor/artifacts/pdfs/fosdem-2024-mutation-zh.pdf` |
| nist-sp-800-218, public domain | 36, Word report | `/opt/cursor/artifacts/pdfs/nist-sp-800-218-zh.pdf` |

Renders: `/opt/cursor/artifacts/corpus-loop/`.

### Fixes in this change

| defect | where it showed |
| --- | --- |
| A text array ended in one content stream and `TJ` began the next, so the lecture text was dropped. | OCW extract |
| A WinAnsi font declared ToUnicode codespace `<0000> <FFFF>` with 1-byte `bfchar` keys. Pairs of bytes became one character (`Special` → `STp⁓ecial…`). Simple fonts now split one byte at a time. | NIST title page |
| A Chinese aside that contained `2000` was one unbreakable citation, then one glyph per line. A sticky span wider than the line breaks on an ordinary boundary. | OCW additive-combinatorics paragraph |
| `version. We write [N]…` and `to x + y = z.` stayed English because a formula on the next line blocked the join. A lowercase prose tail still joins. | OCW page 1 |
| A short wrap just above a theorem frame (`version. We write…`, `to x + y = z.`) was a figure label. A wrap that continues the body paragraph stays in that paragraph. A diagram label (`Retriever`) stays inside the drawing. | OCW theorem boxes |
| Icon-font bullets (U+E902) were not list markers, so three agenda items reflowed onto one line. Private-use bullets stay separate items and paint as `■`. | FOSDEM page 4 |
| `Hi!` wrapped to `你` / `好!`. A title of at most four characters may shrink to half the source size to stay one line. | FOSDEM page 2 |
| `How Mutation Testing Got Practical` and `Software Engineering Consultant` were restored as personal names. A function word, `versus`, `theorem`, or a word longer than nine letters is a title. `Dustin Archibald` and `Jean-Luc Picard` stay names. | FOSDEM title; OCW `Finitary Versus Infinitary` |
| Emoji and icon-font pictures (✅, ❌, 👽) and the fi ligature had no CJK glyph, so the line stayed English. They stand in for `✓`, `×`, `•`, and `fi`. | FOSDEM |
| The rotated footer `This publication is available free of charge from…` shared a baseline with the changelog and the line stayed English. A sideways text matrix is a margin stamp, not part of the sentence. | NIST page 36 |

### ocw-18-225-lec01

| page | findings | severity | fixed |
| --- | --- | --- | --- |
| 1 | Body is Chinese, including `有限与无限`, `下面是一个等价的有穷形式。我们记 [N]…`, and `则 x + y = z 存在单色解`. Display formulas stay math. No word box is off the page. | was high | formula tail; theorem-frame wrap; title is not a byline |
| 2–10 | Body is Chinese. Theorem statements are Chinese. No long English prose run. | — | — |
| 11 | License line is Chinese. The OCW URL stays a URL. | — | — |

### fosdem-2024-mutation

| page | findings | severity | fixed |
| --- | --- | --- | --- |
| 1 | Title is `变异测试如何变得实用`, on three lines, matching the three source lines. `FOSDEM '24` stays Latin. | was high | not a personal name |
| 2 | `你好!` is one line. `Jan-Jelle Kester` stays Latin. `软件工程顾问` is Chinese. | was medium | one-word title; role is not a name |
| 4 | Each agenda bullet is its own line (`■为什么我们需要理解我们的测试`, `■什么是变异测试`, `■变异测试如何达到实际适用性`). | was high | icon-font bullet |
| 20 | Mutation table stays the source operators. The heading `常见突变` is Chinese. | — | table cells |
| 21 | `■✓ 已杀死` and `■• 存活` are each one line. | was medium | list items; emoji stand-in |
| 55 | Language names stay Latin. The heading `针对您所选语言的变异测试` is Chinese. | — | — |

### nist-sp-800-218

| page | findings | severity | fixed |
| --- | --- | --- | --- |
| 2 | Title and affiliations are Chinese. `Murugiah Souppaya` and `Karen Scarfone` stay Latin. The DOI footer stays English. | was high | one-byte WinAnsi |
| 9 | Contents page stays English (`Table of Contents`, `1 Introduction`). | low | toc |
| 10 | Introduction is Chinese, including the superscript note. The rotated footer is still the English DOI line, in the margin. | — | sideways footer |
| 11 | Nested bullets are Chinese, including the wrap under `物联网（IoT）`. Footnote 2 is Chinese. | — | — |
| 14 | Practice prose is Chinese. Column headers and `Example 1:` cells stay English. | — | table cells |
| 29–32 | References stay English. | — | — |
| 36 | Changelog entries are Chinese (`添加了通过公众意见建议的众多示例`). The DOI footer stays in the margin. | was high | sideways footer no longer joins the line |
