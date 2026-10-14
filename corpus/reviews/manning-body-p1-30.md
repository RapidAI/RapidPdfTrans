# Manning body pages 1–30 review

Sample: printed pages 1–30 of *AI Agents and Applications*, starting at the Part 1 opener. That opener is PDF page 29 of the full book. The 30-page slice is PDF pages 29–58 (531×636.572 pt). Source slice: `/opt/cursor/artifacts/pdfs/manning-body-p1-30-source.pdf`. Translation: `/opt/cursor/artifacts/pdfs/manning-body-p1-30.pdf`.

Coverage is complete (`english_body` 0; kept 7126, rewritten 56198, total 63324). Body text is Noto Serif CJK SC Light. Headings are a heavier CJK face and larger than the body. Figure interiors, diagram labels, and monospace listings stay English. Captions are translated. Paragraphs use a 2em first-line indent.

Each translated page was rendered beside the source. Word boxes on all 30 pages have no overlap (vertical overlap above 1.2 pt and area above 40% of the smaller word).

## Fixes in this pass

| Defect | Where it showed | Fix |
| --- | --- | --- |
| `1.1.1` split so `1.` moved with the title and `1.1` stayed on the English baseline | p4, p8, p20, p22 | Echoed digits are wider than the source advance. Atoms of one index still join when each origin steps forward by less than an em, then the whole index moves with the title. |
| A definition line ending in `thou-` joined a narrower label column, so `映射为` / `到` read as two paragraphs | p5 | A hyphenated line joins the closest column margin, not the first column within 64 pt. The definition is one paragraph. |
| `DEFINITION` and `NOTE` left in English | p5, p17 | A source that starts with those labels paints `定义：` or `注意：`. |

## Accepted

- p2 chapter divider is the source numeral `2` plus `第` (from `CHAPTER`). They sit about 180 pt apart and do not overlap. The large digit is the source graphic.
- p13 two Humanist callouts above the class diagram (`The splitter splits…`, `LLM applications process…`) are translated. They sit about 34 pt above the box cluster and are narrower than a body line, so they are not inside the figure. The boxes, class names, and arrows stay English. The caption is translated.
- Short last lines of a paragraph (`因素。`, `信息。`, `到增强。`) are paragraph endings, not a word split across a line.
- License line is `授权给 THIAGO BANDEIRA <…>` on every page. The name stays Latin.

## Per page

| Page | Result | What was checked |
| --- | --- | --- |
| 1 | Pass | `第 1 部分` and `LLM 入门` do not overlap. Body uses the 2em indent. `（LLM）` and `（RAG）` stay. |
| 2 | Pass | Chapter marker is `2` and `第`, same roles as the source numeral and `CHAPTER`. No overlap. |
| 3 | Pass | Chapter title `AI 智能体及应用简介` is one line and ends on the source right edge. `本章涵盖` and the four bullets are Chinese. |
| 4 | Pass | `1.1` and `1.1.1` share the title baseline (`1.1.1` at x=57.2, title at x=93.2, y=401). Body indent and the three category bullets hold. |
| 5 | Pass | Figure 1.1 labels stay English. Caption is Chinese. `定义：嵌入是…映射到一个连续的高维空间` is one paragraph, first line indented, continuations flush. |
| 6 | Pass | `定义：检索增强生成（RAG）…` and `如图1.2`. Figure 1.2 step labels stay English. |
| 7 | Pass | Section `1.1.2` stays with its heading. No leftover English paragraph. |
| 8 | Pass | Figure 1.3 labels stay English. Caption is Chinese. `1.1.3 AI 智能体` is one heading. |
| 9 | Pass | Figure 1.4 labels (`Summarize: War And Peace.PDF`) stay English. The four numbered agent steps are Chinese. |
| 10 | Pass | Figure 1.5 step labels stay English. Caption and the closing paragraphs are Chinese. |
| 11 | Pass | `1.2 介绍 LangChain`. The bullet list is Chinese. `模板` at the foot is the last word of the paragraph that continues on the next page. |
| 12 | Pass | Continuation opens with `以及结构化的链式组合`. `1.2.1` stays with the heading. |
| 13 | Pass with one accepted miss | Figure 1.6 box labels stay English. Caption is Chinese. The two callouts above the drawing are Chinese; see Accepted. |
| 14 | Pass | Body continuation and the component list (`文档加载器`, `文本分割器`, `检索器`) are Chinese. |
| 15 | Pass | `1.2.2` stays with the heading. List markers stay. |
| 16 | Pass | Figure 1.7 class names and the English callouts inside the drawing stay English, including `example, a splitter splits…`. Caption `图1.7` is Chinese and sits under the figure. |
| 17 | Pass | `注意：由于您将构建…` (the source `NOTE`). Class names in the list stay Latin, as identifiers. |
| 18 | Pass | Figure 1.8 class names and the English notes inside the drawing stay English. Caption is Chinese. |
| 19 | Pass | `1.3 LLM 的典型用例` is one heading. The GoDaddy quotation sits on the next line under its title, with `https://mng.bz/8X52` intact. `Medium` stays Latin. |
| 20 | Pass | `1.4`, `1.4.1`, and `1.4.2` each share the title baseline. The technique list is Chinese. |
| 21 | Pass | Figure 1.9 step labels stay English. Caption is Chinese. |
| 22 | Pass | `1.4.3 微调` shares the title baseline. `定义：当 LLM 生成不正确…` has the colon. `在高风险情况下` has no stray space. |
| 23 | Pass | Body and the hallucination discussion are Chinese. No overlap. |
| 24 | Pass | Section prose. Names that are products stay Latin inside Chinese sentences. |
| 25 | Pass | Headings `摘要` are short titles, not orphaned body words. |
| 26 | Pass | Closing prose of the chapter. Footer folio stays the source digit. |
| 27 | Pass | Chapter title `以编程方式执行提示词` is one line from x=262.8 to the source right edge 474.2. |
| 28 | Pass | Both requirement bullets start at the source indent (x=108.8 and x=111.1). `2.1` and `2.1.1` stay with their headings. |
| 29 | Pass | Shell listing stays English. The prose around it is Chinese. |
| 30 | Pass | Python and the `ChatCompletion` dump stay English, including the English string inside the JSON. The surrounding steps are Chinese. |
