# Manning body pages 1–30 review

Sample: printed pages 1–30 of *AI Agents and Applications*, starting at the Part 1 opener. That opener is PDF page 29 of the full book. The 30-page slice is PDF pages 29–58 (531×636.572 pt). Source slice: `/opt/cursor/artifacts/pdfs/manning-body-p1-30-source.pdf`. Translation: `/opt/cursor/artifacts/pdfs/manning-body-p1-30.pdf`.

Coverage is complete (`english_body` 0; kept 7134, rewritten 56190, total 63324). Body text is Noto Serif CJK SC Light. Headings are a heavier CJK face and larger than the body. Figure interiors, diagram labels, and monospace listings stay English. Captions are translated. Paragraphs use a 2em first-line indent.

This export is `manning-body-p1-30-v9.pdf`, replayed from the same binary as pages 31–60 v6. A sentence that crosses a page stays one paragraph: the continuation keeps the body colour and the source x, with no extra first-line indent, and `法律`, `所有`, and `编排` are not cut at the break. A hanging prompt label is the stem only (no colon), right-aligned, and slightly smaller than the sample. This slice has no hanging prompt column and no title-case `Text` label. A prompt line that does not fill its measure is not letter-spaced. Callouts still paint `定义：` and `注意：`. `Tj100` count is 0, and no word `文字` sits in the label margin.

Cross-page paragraphs (printed pages): 11–12, 20–21, 22–23, 23–24, 24–25, and 28–29.

Each translated page was rendered beside the source. Word boxes on all 30 pages have no overlap (vertical overlap above 1.2 pt and area above 40% of the smaller word). No page paints a one-character translation of a lone CHAPTER or PART label, and no numbered step repeats its number or indents the continuation differently from the hanging edge.

## Fixes in this pass

| Defect | Where it showed | Fix |
| --- | --- | --- |
| `1.1.1` split so `1.` moved with the title and `1.1` stayed on the English baseline | p4, p8, p20, p22 | Echoed digits are wider than the source advance. Atoms of one index still join when each origin steps forward by less than an em, then the whole index moves with the title. |
| A definition line ending in `thou-` joined a narrower label column, so `映射为` / `到` read as two paragraphs | p5 | A hyphenated line joins the closest column margin, not the first column within 64 pt. The definition is one paragraph. |
| `DEFINITION` and `NOTE` left in English | p5, p17 | A source that starts with those labels paints `定义：` or `注意：`. |
| Lone `CHAPTER` on the blank verso became `第` | p2 | A CHAPTER/PART label with no title stays as the source. It does not become one CJK character. |
| Chapter title ran through the giant numeral and split `智能体` | p3, p27 | A two-line title stops short of the numeral path. A Latin lead stays on the first line (`AI` / `智能体及应用简介`). |
| Numbered steps after Figure 1.4 were headings | p9 | A number plus a hanging indent is one body paragraph. The number keeps its source face, colour, and x. Every line of the words shares the hanging indent, with no first-line indent. A heading cannot end on `的` or `，`. |
| Prerequisite bullets disagreed: one black square, one larger gold square, and two text indents | p28, and the same list shape on the chapter openers | Every dingbat in a list is redrawn from the source glyph, colour, size, and x. Every line of the words starts at the source hanging indent. A wrapped item uses the same mark as a one-line sibling. |
| `思维链（` left the opening bracket at the end of the line | p28 | An opening bracket cannot end a line. It moves onto the next line with the text it opens. |
| A shorter paragraph left about five blank lines above the shell listing, and the paragraph under the listing stayed on the English baseline | p28, and the same shape on p29–p30 | Kept code is a block in the column. When the prose above gets shorter, the listing and the following paragraph move up so the gap stays the source gap. A prompt arrow or dingbat on that baseline moves with the code. |
| `如果对这些任务不熟悉…配置` stopped early, and `（CoT）。…PromptTemplate 和` stopped before `FewShotPromptTemplate` | p28 | A body line that is not the last line of its paragraph fills by reflow. A long Latin token breaks at a camelCase boundary (`Jupyter` / `Notebook`). CJK–CJK justification stays within 0.15em. |
| Moved listings dropped spaces and the WinAnsi apostrophe in `don’t` | p28–p30 | The redrawn listing uses the source character codes, including spaces and bytes above 127, and keeps a mark that sits on the same baseline. |

## Accepted

- p2 is a blank verso. The only visible ink is the license line. The text layer still has the source `2` and `CHAPTER`; those marks do not rasterize, and they are not translated.
- p13 two Humanist callouts above the class diagram (`The splitter splits…`, `LLM applications process…`) are translated. They sit about 34 pt above the box cluster and are narrower than a body line, so they are not inside the figure. The boxes, class names, and arrows stay English. The caption is translated.
- Short last lines of a paragraph (`因素。`, `信息。`, `到增强。`) are paragraph endings, not a word split across a line.
- License line is `授权给 THIAGO BANDEIRA <…>` on every page. The name stays Latin.

## Per page

| Page | Result | What was checked |
| --- | --- | --- |
| 1 | Pass | `第 1 部分` and `LLM 入门` do not overlap. Body uses the 2em indent. `（LLM）` and `（RAG）` stay. |
| 2 | Pass | Blank except the license line, matching the source. No lone `第`. |
| 3 | Pass | Title is two lines, `AI` then `智能体及应用简介`, both ending at x=384.8. The giant `1` starts near x=428, so the title does not cross the stem. The four `本章涵盖` bullets are one mark: x=96.2, size 8.46, and the words start at x=108.2. |
| 4 | Pass | `1.1` and `1.1.1` share the title baseline (`1.1.1` at x=57.2, title at x=93.2, y=401). Body indent and the three category bullets hold. |
| 5 | Pass | Figure 1.1 labels stay English. Caption is Chinese. `定义：嵌入是…映射到一个连续的高维空间` is one paragraph, first line indented, continuations flush. |
| 6 | Pass | `定义：检索增强生成（RAG）…` and `如图1.2`. Figure 1.2 step labels stay English. |
| 7 | Pass | Section `1.1.2` stays with its heading. No leftover English paragraph. |
| 8 | Pass | Figure 1.3 labels stay English. Caption is Chinese. `1.1.3 AI 智能体` is one heading. |
| 9 | Pass | Figure 1.4 labels stay English. `考虑以下示例` is a body paragraph with a 2em first-line indent (x=120.1, then x=102.2). Steps 1–4 are one paragraph each: the gold number stays at x=117.2, and every line of the words starts at x=129.8 in the body face. |
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
| 27 | Pass | Chapter title is two lines, `以编程方式` then `执行提示词`, both ending at x=360.5. The giant `2` starts near x=398, so the title does not cross it. The four `本章涵盖` bullets match page 3: x=96.2, size 8.46, words at x=108.2. |
| 28 | Pass | Both requirement bullets are the same gold square at x=108.8, size 8.46, on the item baseline. Both word lines start at x=120.8. The first paragraph ends `例如思维链` and the next line starts `（CoT）。`, reaching x=435 (92% of the column) before `FewShotPromptTemplate`. `如果对这些任务不熟悉…配置` reaches the column edge at x=465. The shell line sits 18.8 pt under `入 ch02 文件夹：`, the source gap, and `python -m venv env_ch02` keeps its spaces. `2.1` and `2.1.1` stay with their headings. |
| 29 | Pass | Shell listing stays English, including the `➥` prompt marks on the `pip` and `jupyter` lines and the spaces in `(env_ch02) C:\…`. The prose around it is Chinese. |
| 30 | Pass | Python and the `ChatCompletion` dump stay English, including spaces and the apostrophe in `don’t`. The surrounding steps are Chinese. |
