# Corpus loop: math paper, slide deck, scanned pamphlet

Three new documents, each a different layout. Translated English to Chinese on current main. Figure interiors, display math, and name lists stay in the source language. Coverage `english_body: 0` and `complete: true` is the pass bar. The PDFs are not committed.

| id | pages translated | PDF |
| --- | --- | --- |
| arxiv-2609.15979, The k-server conjecture is true | 18, letter, Latin Modern | `/opt/cursor/artifacts/pdfs/arxiv-2609.15979-zh.pdf` |
| fosdem-2025-welcome | 36, landscape 720×405 | `/opt/cursor/artifacts/pdfs/fosdem-2025-welcome-zh.pdf` |
| lafollette-illusion-1924 | first 12 of 52, OCR text layer | `/opt/cursor/artifacts/pdfs/lafollette-1924-p1-12-zh.pdf` |
| arxiv-2410.22360, ArxivDIGESTables | 21, A4, two-column | `/opt/cursor/artifacts/pdfs/arxiv-2410.22360-zh.pdf` |

`arxiv-2609.15979` was already in `corpus/manifest.json`. The FOSDEM 2025 deck and the LaFollette OCR text layer were added there (URL, license, sha256). Manning body slices stayed regression-only: `rpt-core` lib tests, including the definition join, the NOTE callout, and the listing annotation, still pass (397 passed, 1 ignored).

## Fixes

| defect | where it showed |
| --- | --- |
| A 36pt title on the next slide continued a 24pt ○ bullet. The size band was measured on the larger face, so 36 vs 24 still counted as one item. | FOSDEM page 2 dinner bullet ran into page 3, “如果需要帮助 / 谁负责什么”. |
| `Your Ideas` is title case, so the byline restore put the English headline back. | Scan page 2. |
| A margin column of `§`, `\|`, `1` was already dropped when the mark stood alone. OCR glues a letter on (`\|\|`, `j§`, `jj\|`) and leaves a stray `S` in the same column, so the sentence was translated with the ornament. A full-width row of `l` from a printed rule was redrawn as visible text. | Scan page 2 ad. |

The slide title now has to stay within 1.2× the list line. A rule column is the repeated x, including a short run glued to a rule character. A stray letter on that column is dropped with the rules. A spaceless line that is mostly `l` and `|` is not a sentence.

A second pass closed the three gaps that pass left open:

| defect | where it showed |
| --- | --- |
| Alpha keys `[AGTG21]` and `[AHK+ 02]` were not bibliography entries, so titles were translated. A following sentence with “and” kept the detector from opening the section. | k-server pages 14–17. |
| `Proof.` is its own draw on the paragraph’s first baseline. Closing the hole above the paragraph left `证明。` between 特 and 别. The body indent then started under the label. | k-server page 8. |
| The cover’s OCR (`POBUtfttCO`, `3jOV£9rOIit`) was painted as Chinese over the photograph. | Scan page 1. |

An alpha key is two or more letters, optional `+`, and a year, inside brackets. The section still stops at `A Omitted proofs`. A one-line `Proof.` moves with the paragraph, and the first line starts at the source x of the word after the label. A page with no sentence and at least two junk tokens is not translated; a later page that contains a sentence still is.

## k-server (18 pages)

Body, lemmas, and the appendix proofs are Chinese. Display math, subscripts, and citation keys stay as set. Coverage is complete.

- Pages 1–3. Title and abstract in Chinese. Christian Coester stays Latin; Elias Koutsoupias and Marek Zbysiński are translated because the surname is long or not ASCII. Definitions and the proof sketch read as prose around the formulas.
- Pages 4–13. Theorems and proofs in Chinese. Formulas stay formulas. On page 8, `证明。` shares the first baseline with `初始化` and ends before that word (x 72–101, body from 106). `特别是` stays one word on the next line.
- Pages 14–17. The bibliography stays English, including `References`, `[AGTG21]`, and `Chasing convex bodies`. No Chinese on page 14.
- Page 18. Appendix heading and proofs in Chinese (`A 省略的证明`, `证明。由工作函数递推`), with the displayed equations kept.

## FOSDEM 2025 welcome (36 pages)

Coverage complete: kept 4630, rewritten 3886, english_body 0. ○ items are separate, and the checked pages are not letter-spaced.

- Page 1. 欢迎！
- Page 2. Volunteer bullets, including the two ○ items. The dinner bullet ends on this page.
- Page 3. Title is the whole line 如果需要帮助 / 谁负责什么, then the shirt colors.
- Page 4. A lone URL.
- Page 5. 信息台, with ○ 地图 / 捐赠 / 问题.
- Pages 6–8, 10–12. Lost and found, network, social, schedule counts. URLs and the tag stay Latin.
- Page 9. No text.
- Pages 13–18. Devroom, stand, and BoF name lists stay English. The Chinese headings are 日程 开发者会议室, 日程表 展位, and 日程 - BoFs. One Chinese fragment is mixed into the first name list.
- Pages 19–23. Hacker-room note, etiquette, the 25-year t-shirt (开始时间：14:30, two ○ items), dashboard, anecdotes.
- Pages 24–27. 寻宝游戏, 全球切换日, 礼仪, feedback.
- Pages 28–30. Health, 急救, 行为准则. A short bullet tracks at the 0.15em cap, so a text extract shows spaces inside 流感是真实存在的; the advance is that cap, not a joined bullet.
- Pages 31–36. Thanks, sponsors, donate, 你！ (the 56pt title on the photo), 玩得开心, 彼此善待.

## LaFollette Illusion, pages 1–12

The source is an InvisibleOCR text layer. Painted text is the review. `pdftotext` still extracts the invisible rule glyphs on the advertisement. Coverage is complete, english_body 0. Pages 13–52 of the file were not translated.

- Page 1. The cover is the photograph only. The OCR tokens are kept and not painted, so there is no Chinese overlay. A text extract still sees the invisible layer (`TOILETTE`, `POBUtfttCO`); the painted page does not.
- Page 2. The ad paints in Chinese. 您的想法, the Daily Worker lines, the price, and the Chicago address. No `\|`, `§`, `j§`, or `S` in the painted lines. The rule row of `l` is not redrawn.
- Page 3. Title page in Chinese. Jay Lovestone is translated. “Coolidge” stays.
- Page 4. Blank.
- Pages 5–6. Preface in Chinese. One sentence still contains the English word “effects”.
- Page 7–12. Chapters 1 and 2 in Chinese. The heading Business Interests Safe is 商业利益得到保障. “J. P.” style names that are real people stay Latin when the line is a name.

## Owner pass

Exports below replace the three files from the first pass. Coverage is `english_body: 0` and `complete: true` on each. `rpt-core` lib tests: 353 passed, 1 ignored.

| id | PDF |
| --- | --- |
| arxiv-2609.15979 | `/opt/cursor/artifacts/pdfs/arxiv-2609.15979-zh.pdf` |
| fosdem-2025-welcome | `/opt/cursor/artifacts/pdfs/fosdem-2025-welcome-zh.pdf` |
| lafollette-illusion-1924 v2 | `/opt/cursor/artifacts/pdfs/lafollette-1924-p1-12-zh-v2.pdf` |

### k-server, pages 4–5 and the other 16

A bold run-in is one unit with the paragraph. `相关问题。`, `工作函数和 WFA。`, `次凸性、赋值拟阵和热带几何。`, and `随机化 k-服务器。` are bold, and the body continues on that same baseline. The italic math `k` in `Randomized k-server.` stays inside the run-in. Word boxes on pages 4 and 5 do not overlap.

`O(log k)`, `Ω(log k)`, `Ω(log² k)`, `O(log n log² k)`, `O(log ∆ log³ k)`, and `n = k + 1` stay in the sentence. `Plücker` is one word. `[CK19, GKP24, CP26]` closes the k-taxi sentence instead of sitting alone under a hole. A citation key such as `GKP24` is not scored as OCR. CJK justification stops at 0.05em, so `对于任意` is not tracked out into `对 于 任 意`.

Pages 14–17 stay an English bibliography. One opening paragraph on page 1 has a single pair of lines about 4pt apart (`许多最初为` over `服务器位于`). A few formula lines still contain English (`For instance`, `The` beside a lemma number).

### FOSDEM welcome, pages 13–14 and the other 34

The devroom list is Chinese, centered, with product and project names left in Latin (`Ada`, `Android Open Source Project`, `LLVM`, `RISC-V`, `PostgreSQL`, `WebAssembly`, `Matrix.org`). Topic words are translated (`能源`, `法律与政策`, `开放媒体`, `机密计算`). The first letter of a long centered line is not a margin stamp, so the shared text operator is rewritten. Coverage counts an all-English comma list as `english_body`. Page 14 is the same list continued; a short tail (`子计算、无线电、铁路`) is centered, then the next chunk continues.

### LaFollette Illusion v2, pages 1–12

`/opt/cursor/artifacts/pdfs/lafollette-1924-p1-12-zh-v2.pdf`. Each text block is scored (dictionary-word ratio and garbage-character ratio). A low-quality block is not translated and not painted. A title scan that also contains a bad block drops every text block, so the photograph stays.

- Page 1. The cover is the photograph. There is no `洗手间`, no `幻象`, and no painted `3jOV£9rOIit`. A text extract still sees the invisible OCR layer (`TOILETTE`, `3jOV£9rOIit`).
- Page 2. The advertisement is Chinese (`您的想法`, the Daily Worker lines, the price, the Chicago address).
- Page 3. Title page in Chinese. Jay Lovestone is translated.
- Pages 4 and 6. Blank scans.
- Page 5. `前言`.
- Pages 7–12. Chapters 1 and 2 in Chinese, including `商业利益得到保障` territory and the Hoan quotation.

## Owner pass: column fill and the binomial paragraph

`rpt-core` lib tests: 374 passed, 1 ignored. Coverage on both exports is `complete: true`, `english_body: 0`.

| id | PDF |
| --- | --- |
| arxiv-2609.15979 | `/opt/cursor/artifacts/pdfs/arxiv-2609.15979-zh.pdf` |
| fosdem-2025-welcome | `/opt/cursor/artifacts/pdfs/fosdem-2025-welcome-zh.pdf` |

A prose `，` stays on the line unless the break already keeps 90% of the fitted span. A name list (two or more `、`) may still break at `、` from 60%. A parenthetical that wraps bracket keys (`（例如参见 Speyer … [SS04] 以及 … [MS15]）`) is prose and wraps; `(Smith et al., 2020)` stays one token. The ragged page-4 lines (`例如，` / `然而，` / `[DW92] 表明，` / `进一步扩展`) came from those two breaks, not from the binomial keep.

The binomial is one placeholder in the sentence (`一个` ⟦k+1 over 2⟧ `维`). Inline math stays in the translation (`µ(G) = min…`, `(k + 1)`). The page 9 section heading is painted in place. A numbered heading still follows a pull that only closes empty space (`1 引言` stays with the shorter abstract). It does not inherit a pull that would cross a formula the paragraph above left on its old baseline. A slide bullet is not OCR garbage. A centered comma list is not the next line of the previous slide. `真实` is one compound. A citation `+` is not its own baseline.

### FOSDEM, all 36 pages

Pages 13 and 14 stay one centered list each. Page 13 includes Ada, GraphQL, OpenAPI, AsyncAPI, DNS, eBPF, Go, HPC, JavaScript, LibreOffice, LLVM, and Matrix.org 基金会与社区. Page 14 includes Mozilla, MySQL, Nix, NixOS, CAD/CAM, BMC, Bootloader, PostgreSQL, Python, `量子计算`, RTC, RISC-V, Rust, SBOM, Swift, WebAssembly, Web 性能, and `文档工具化`. Topic words are Chinese.

Page 7 is Chinese (`网络`, `仅 IPv6`, `修好你的 bug`, NAT64/DNS64). Page 12 is the Chinese schedule (`1052 活动`, `933 位演讲者`, `70 个主题轨道/Devroom`) and does not absorb page 13. Page 28 breaks as `流感是` / `真实存在的`. Page 31 is `谢谢` / `所有志愿者`. Pages 15–16 are booth-name lists. Remaining Latin on the other slides is URLs and product names. Page 9 has no text.

### k-server, all 18 pages

Page 4. The opening paragraph is Chinese. The stack sits between `一个` and `维`, numerator `k+1` over `2`. Superscripts and subscripts keep the source glyph size and offset: `q` is 9.7pt with `x` at 7.1pt below it, `z` is 9.7pt with `−d(x,y)` at 7.5pt above it, and `g` carries `i` at 7.1pt with a nested `1` at 5.3pt. Each formula hole is the math ink width in points, from the leftmost origin to the rightmost pen, and that width is what wrap, fit, justification, and paint share. Adjacent holes no longer collapse into one run, and a hole at the end of a line is not trimmed. Section 1.3 and the run-in paragraphs fill the column: measured body lines are 90–103% of the 468pt measure except paragraph-final lines (`分别在引理 5.1…`, `表示）。`, `无限服务器问题…`, `[Sit14] 的候选方案。`, `M♮-凹函数…[Mur03]。`). `例如，分页`, `然而，`, and `[DW92] 表明` are full lines. The Plücker sentence continues through `（例如参见 Speyer 和 Sturmfels` and breaks at 96% of the column. `Plücker` is one word. `(k + 1)` stays inside the parentheses.

Page 1. Body lines are 13.5pt apart. `许多最初为` over `服务器位于` is that same leading. `[DEH+17]` does not open a gap.

Page 9. `对于具有方形对角块的矩阵` and `固定有限标签集 … N ×` are Chinese, with `n+1` over `2` and `k+1` over `2` on that line (pdftotext y=483.6 beside N at y=486.4). The section heading is `4 请求时的势及其增量`, bold, 13.8pt, at x=72 and y=458.6, on the source baseline. U∗/0W stays at y≈400, x≈339. Lemma 3.2 items 1 and 2 are Chinese with the original `µ(AG)` and `µ(F)` glyphs. Item 3 is Chinese; the block-diagonal display (`G_1,…,G_s` and the sum) is wider than the hole and sits on the proof line. The proof is Chinese (`每个所选行列式都乘以 detA`). A repeated `⟦0⟧` is drawn the second time as the formula's unicode, so that paragraph is no longer left in English.

Pages 5–8 and 10–13. Formula paragraphs are Chinese around the holes (`引理 2.2`, `引理 2.3`, the valuation sentences). A display line that is only the formula stays in the source glyphs (`f = Σ f_a z^a` on page 5, the two `ExtCost` lines and `(28)` on page 13). Pages 14–17 stay an English bibliography. Pages 2–3, 6, and 18 are Chinese around the formulas. `随机化 k-服务器` on page 5 is Chinese, with `O(log k)` and `Ω(log² k)` in the sentence.

## arXiv:2410.22360

`rpt-core` lib tests: 397 passed, 1 ignored. Coverage on the export is `complete: true`, `english_body: 0` (kept 21132, rewritten 36915). The paper is in `corpus/manifest.json` (CC BY 4.0, sha256 `bb5c112655f8be1aa998a9cb7c935b0e6ce20913513b6dd147400f3623c45ad4`, URL `https://arxiv.org/pdf/2410.22360.pdf`). Export: `/opt/cursor/artifacts/pdfs/arxiv-2410.22360-zh.pdf`.

A chart label drawn on top of a body line at a different size is peeled off, so page 4’s right column reads as Chinese (`物理学、定量生物学、统计学`) and the figure banners (`Highest quality`, `Filter out misformatted tables`) stay on the artwork. A caption word space before small caps is not a column gutter, so `表 2：ARXIVDIGESTABLES 列中的方面类型。` is painted. A one-line bullet longer than 30 letters is not chained into the formula: page 6’s scoring list is Chinese. The formula stays in the source face, and the English tail is the clause on that baseline: `ϕ(a)=ϕ(a)，否则为 0。` at y=222.1, with `m`/`r` above and `i`/`j` below. Page 3’s `§6` stays on the sentence baseline (`最后，在§6中`), and the footnote expansion is `文档信息收集与科学表格抽取`. Page 9’s table headers are `挑战类型`, `描述`, `复杂方面`, and `过于具体`. Page 13’s lead-in is `下面是一个示例实例来自` at y=96.2; an 11pt icon under the first word is not figure ink. `计算机科学` is Chinese beside `1985`. The other field names in that table stay source (`Physics`, `Statistics`, `Math`, and the quantitative rows) because the cells were not segments. The JSON example and the prompts stay English. `References` in the right column opens on the author roster (`OpenAI Josh Achiam…`, `Ojas Ahuja…`); pages 10–12 stay an English bibliography, and the left-column Limitations section stays Chinese. The next page’s left column continues the bibliography. `A Data Processing` stops the section.

Manning stays regression-only. The same suite (397 passed, 1 ignored) includes the definition join, the NOTE callout, and the listing annotation. The full book was not translated. The Manning source is not in `corpus/cache`, so this pass did not replay the body slices.

## k-server holes and hats

`rpt-core` lib tests: 395 passed, 1 ignored. Coverage on `/opt/cursor/artifacts/pdfs/arxiv-2609.15979-zh.pdf` is `complete: true`, `english_body: 0` (kept 11411, rewritten 27374). The file has no `■`, U+FFFD, or `.notdef`. 100dpi renders of all 18 pages were checked against the glyph extract.

Page 4. The opening paragraph’s holes are the math width, so the line continues after `q_x`, `q_y`, `(i,j)`, and `索引`. Non-final lines fill at least 90% of the 468pt column. Nothing crosses x=540. `表示）` stays on the paragraph’s last line, and `z^{−d(x,y)}` stays inside the column.

Page 5. `随机化` (x 72–105) then `k-服务器。` (x 107–162) then `k-server` (x 166). The body starts after the label. A formula gap does not freeze that body under a longer Chinese label.

Page 9. The heading `4 请求时的势及其增量` is bold at x=72, y=322.3, on the source baseline (source `4` is at y=322.3). The paragraph above ends at y=424.0 (`值为两个块赋值之和。`). The body `固定有限标签集` starts at y=298.0. The display `Q = [z q q], Ψ = µ(Q)` stays at y=259.5. A numbered heading still closes an empty hole, and inline math in the next paragraph does not pin it. A display equation under the heading does.

Page 10. Latin Modern `\widehat` (U+F8F8) is part of the formula hole, so `Q̂`, `Ĥ`, and `µ(Q̂)` are the original glyphs. A hat’s hole is the ink past the letter on that same baseline. A letter on the next line does not widen it. `将 q̂` then `x` (pen 127.8) then `视` at x=131.3, a normal word space. The second `q̂_x。` has `。` 1.0pt after `x`. `ν(det Q̂*)` then `=` 1.3pt after the closing parenthesis (source ink gap before `=` is 3.0pt). `由于 q_r = e₁，` keeps `e` on the baseline, `1` at y=426.5, and `，` at y=428.9, x=509.2, 0.4pt past the digit. Lemma 4.1: `引理 4.1.` ends at x=116.3 and `请求` starts at x=120.6. `+` is on the formula baseline y=505.2, 2.5pt after `)` and 2.5pt before `µ` (source sidebearings are 2.4pt). Lemma 4.2 is the same shape at y=168.2, with the prime still at y=172.2. On the first line, `e_i e_j` is one hole: math `e` at x=306.7 with subscript `i` at x=311.7, math `e` at x=315.1 with subscript `j` at x=320.2, then `组` at x=326.2. There is no stray text `e` and no floating `j`.

Page 11. A one-line formula paragraph used to stamp its last character onto the math, because the coverage box measured a hole as two digits and an ideographic space. The box now uses the laid-out width, so `写出` no longer sits on `q̂_y`.

Pages 1–3, 6–8, and 12–18 match the previous export except where a script stamp moved into its formula. Pages 14–17 stay an English bibliography. Page 12 no longer has a floating `ij` at the bottom. Hat-on-letter overlaps are the accent sitting on its base.

## Left open

- The scan’s invisible OCR layer is still in the content stream, so a text extract of the LaFollette cover and of page 2 still shows the old characters. The painted cover does not.
- Lemma 3.2 item 3's block-diagonal display is wider than a short inline hole. On the current export it rides with the proof line instead of covering the sentence below.
- On arXiv:2410.22360 page 13, the field-of-study cells other than `计算机科学` and `电气工程与系统科学` stay English. They were kept as table text. The JSON example stays source.
