# Manning pages 1–30 review

Sample: pages 1–30 of *AI Agents and Applications* (531×666 pt). `zh-v31.pdf` replays the v30 translations (the three wording misses were sent to the model again; the rest stayed cached). Copy: `/opt/cursor/artifacts/pdfs/manning-p1-30-v31.pdf`. Each page was checked against the source glyphs for alignment, line breaks, and paragraphs.

30/30 pass those three classes. The wording notes from v29 were rechecked on v31.

## Root causes fixed

| Defect | Where it showed | Fix |
| --- | --- | --- |
| Roman folio painted as Chinese (`ii` → `二`, `vi` → `六`) | p4, p8 | Margin folios stay the source token. |
| Section page numbers slid to the column edge on the old baseline | detailed contents | An inline page number sits on the title's last line. |
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
| 8 | Pass | Footer folio stays `vi`. Part leaders share a right edge. Chapter rows keep their numbers beside the titles. |
| 9 | Pass | Running head `简要目录` and `vii` are separate. Appendix rows are one line each. |
| 10 | Pass | Chapter `1` stays 27pt. Subentries are inline. |
| 11 | Pass | `2.2 …提示词31` and `2.5 …40` sit on the title line. Chapters 2–4 stay 27pt. |
| 12 | Pass | Chapter `5` is 27pt on its own baseline, under `网络研究链99`, not glued to that row. Chapter `6` stays 27pt. |
| 13 | Pass | Section numbers sit on their titles. Chapters 7 and 8 stay 27pt. |
| 14 | Pass | Chapter `10` is 27pt, separate from `10.1`. Wraps read `检索内容使用重写后的查询` and `组合成一个`. |
| 15 | Pass | Chapter `11` is 27pt, separate from `11.1 简单开始：构建单工具旅行信息`. |
| 16 | Pass | Chapters 12–14 stay 27pt and separate from `12.1` / `13.1` / `14.1`. The `和` / `超越327` wrap matches the source break before `beyond`. |
| 17 | Pass | Contents tail. Han joins no longer leave a space in `人在环中循环` or `评估智能体`. |
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

## Wording rechecked on v31

- p8 / p13: `第 4 部分 高级 RAG`. The space inside `高级 检索` is gone. The heading keeps the acronym `RAG`.
- p16: `该解决方案: 模型上下文协议`. The doubled `该` and the English title are gone. Page number `310` stays on that row.
- p25: `以便与普通文本区分开来`. The old join `格式。以将其` is one sentence again.
