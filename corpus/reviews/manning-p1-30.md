# Manning pages 1–30 review

Sample: pages 1–30 of *AI Agents and Applications* (531×666 pt). Wording is the v30 cache. Delivered copy: `/opt/cursor/artifacts/pdfs/manning-p1-30-v35.pdf`. Contents pages (PDF pages 8–17) were compared with the source geometry, not with a right-aligned column.

30/30 still pass alignment, line breaks, and paragraphs. Coverage is complete (`english_body` 0; kept 2151, rewritten 35509, total 37660).

The source does not right-align page numbers. Front matter, chapters, and sections put the number one gap after the title (`1.1 Building LLM-based applications and agents 4`). Only part rows use leader dots out to the page edge. v33 and v34 had followed an earlier right-align instruction and are not the source.

## Root causes fixed

| Defect | Where it showed | Fix |
| --- | --- | --- |
| Roman folio painted as Chinese (`ii` → `二`, `vi` → `六`) | p4, p8 | Margin folios stay the source token. |
| Section and chapter page numbers were parked on a right edge (v32–v34) | detailed contents, p10–p16 | The number sits one gap after the Chinese title. Part rows alone keep leaders and the source edge (p10 `1` at 465.3, p11 `53` at 474.2). |
| Front-matter roman numerals were parked on that same edge (v32–v34) | p10 `前言 xvi` through `关于封面插图 xxvi` | Titles stay at x=141.2 and the roman token follows the title (`xvi` ends at 176.4, `xxvi` at 215.2). |
| Title-page lines sized from the small-caps median and left-ragged | p5 | One line of small caps is set at the cap size and shares the source right edge. |
| `Special Sales Department` left in English | p6 | That label paints as 特别销售部. Role/name colons and the address stack were already on one axis. |
| Han wrap inserted a space (`检索内容 使用`, `组合成 一个`) | contents | Two Han fragments join with no space. Latin still gets a space. |
| 27pt chapter index glued to the section under it, or to the bullet row above | p12 `■5`, p14–p16 `10`–`13` | A large numeral is its own line, and it is not a bullet entry. |
| One sentence split by a 0.0002pt baseline, leaving a hole | p25 | A same-baseline return to the left margin stays one line. |
| `https://` left at the end of a line | p25 | A URL is one token. The index is by character, so CJK before the URL does not shift it. |

## Per page

| Page | Result | What was checked |
| --- | --- | --- |
| 1 | Pass | Cover subtitle breaks after `、`. Author stays Latin. `曼宁` is the wordmark, not a folio. |
| 2 | Pass | Diagram callouts stay English (inside the figure). Caption is translated. Header `2` and `后记` are separate. |
| 3 | Pass | Title only. |
| 4 | Pass | Header folio stays `ii` (v25 had `二`). |
| 5 | Pass | Subtitle 19.2pt, author 11.5pt, 谢尔特岛 9.2pt, all ending on the source right edge 474. Previously the small-caps median painted them at 15.3 / 8.6 / 7.4. |
| 6 | Pass | `特别销售部` and the address rows share x=90.2. Credit labels share the colon axis. Body paragraphs use the 2em first-line indent. |
| 7 | Pass | Dedication only. |
| 8 | Pass | Footer folio stays `vi`. Part leaders share a right edge. Brief-contents chapter numbers stay beside the titles, as in v25 and the source. |
| 9 | Pass | Running head `简要目录` and `vii` are separate. Appendix rows are one line each. |
| 10 | Pass | Chapter `1` stays at x=105.3, 27pt. `AI 代理和应用简介` is bold with the source italic shear, and `3` follows that title. Section indexes `1.1`–`1.6` share x=138.9 and size 11.2; titles share x=165.2 (source 165.2) with the page number inline (`4` after `代理`, not at 465). Front matter stays at x=141.2 with the roman token inline. Sub-entries share x=177.2, including continuations, and stop short of the part edge. |
| 11 | Pass | Section indexes `2.1`–`2.6` share x=147.9 and size 11.2 with `3.1`–`4.3`. Titles share x=174.2 (source 174.2), including `2.1 以编程方式运行提示` and `2.2`. `为本章设置环境` and `使用 LangChain 的 PromptTemplate 33` are one line each at x=186.2. The part row keeps leaders and `53` at 474.2. Chapters 2–4 stay 27pt. |
| 12 | Pass | Chapter `5` is 27pt on its own baseline. Section numbers stay on the column. Packed subentries keep each page number with its title. |
| 13 | Pass | Section numbers share the column. Chapters 7 and 8 stay 27pt. `第 4 部分 高级 RAG`, the long leaders, and `171` share y≈409, as in v31 and v25. |
| 14 | Pass | Chapter `10` is 27pt, separate from `10.1`. Section numbers share the page's 445.7 column. Wraps read `检索内容使用重写后的查询` and `组合成一个`. |
| 15 | Pass | Chapter `11` is 27pt, separate from `11.1`. Section numbers share 474.2. |
| 16 | Pass | Chapters 12–14 stay 27pt and separate from `12.1` / `13.1` / `14.1`. Section numbers share the page column. |
| 17 | Pass | `14.2` / `14.3` page numbers meet at 298.1, the same edge as v25. Inline rows stay in the indent. Han joins stay closed in `人在环中循环`. |
| 18 | Pass | Preface body. First line indented, continuations flush. Folio `xvi` kept. |
| 19 | Pass | Continuation. Indents match page 18. |
| 20 | Pass | Acknowledgements. Latin names kept. |
| 21 | Pass | Name list. |
| 22 | Pass | `本书适合哪些读者` is a heading. Folio `xx` kept. |
| 23 | Pass | Hanging dash list. Part lines keep their markers. |
| 24 | Pass | List continues. Header `xxii` and `关于本书` are separate. |
| 25 | Pass | The code paragraph is one block (the old hole from `like this` / `to separate` is gone). `https://mng.bz/rZlZ` is intact on the next line, and `代码片段` is not stranded. |
| 26 | Pass | Both liveBook URLs stay on one line. The gap before the forum heading is a section break. |
| 27 | Pass | Author bio, block indent. `ROBERTO INFANTE` kept in Latin. |
| 28 | Pass | Cover-illustration note. Folio `xxvi` kept. |
| 29 | Pass | Part opener stacks `第 1` / `部分` the way the source does. |
| 30 | Pass | Running head `2` and `章节` are separate. Chapter 2 has not started its body yet. |

## v35 contents check

Pages 8–17 were measured against the source word boxes.

- Part rows keep leaders and the source number edge (brief contents 453.1–453.2; detailed contents 465.3 or 474.2 by page). No stray `S` sits in the part 1 dots. The small-caps `S` in `LLMS` stays with the title.
- Section indexes occupy the source index x (p10 138.9, p11 147.9). The dot in `1.1` is its own glyph, so the title column is the source title x (p10 165.2, p11 174.2), not a word space after the index. Index and title share one baseline and one size (11.2). Closing a hole above a section moves both draws together.
- Page numbers on front matter, chapters, and sections sit one gap after the title. `3.1`’s `56` is beside the shorter Chinese title, not at the English edge 474.
- Sub-entry rows use the source left per page (177.2 on even contents pages, 186.2 on odd). Continuation lines use that same x. Wrap stays inside the source run’s ink, short of the part-row edge.
- `使用 LangChain 的 PromptTemplate 33` is one line. A title wraps only when that column is full. `LangChain的核心对象模型 15` is one line because the Chinese title fits; the English source wraps.

Brief-contents part 3 (`第 3 部分 问答聊天机器人` / `119`) still has a gap where the source draws leader dots. That gap is the same in v34. The number remains at the source edge 453.1.

## v32 contents check

Pages 8 and 9 match v31 (brief contents and the appendix list). Pages 10–17 were compared with the source and with v25. Column page numbers stay on the title baseline. The only x shifts versus v31 are the hanging continuation markers (~7.8pt into the gutter) and the title text that follows them, which now shares the run's first-line x. Page 11's part row, which v31 had split, is back with its title the way v25 draws it.

## Wording rechecked on v31

- p8 / p13: `第 4 部分 高级 RAG`. The space inside `高级 检索` is gone. The heading keeps the acronym `RAG`.
- p16: `该解决方案: 模型上下文协议`. The doubled `该` and the English title are gone. Page number `310` stays on that row.
- p25: `以便与普通文本区分开来`. The old join `格式。以将其` is one sentence again.
