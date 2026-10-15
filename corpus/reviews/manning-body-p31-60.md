# Manning body pages 31–60 review

Sample: printed pages 31–60 of *AI Agents and Applications*. That span is PDF pages 59–88 of the full book. The 30-page slice is 531×636.572 pt. Source slice: `/opt/cursor/artifacts/pdfs/manning-body-p31-60-source.pdf`. Translation: `/opt/cursor/artifacts/pdfs/manning-body-p31-60.pdf`.

Pages 1–30 are `/opt/cursor/artifacts/pdfs/manning-body-p1-30.pdf` (also `manning-body-p1-30-v6.pdf`). This slice is `manning-body-p31-60-v3.pdf`.

Coverage is complete (`english_body` 0; kept 10090, rewritten 50937, total 61027). Body text is Noto Serif CJK SC Light. Headings are a heavier CJK face and larger than the body. Figure interiors, table cells, and monospace listings stay English. Captions are translated. Paragraphs use a 2em first-line indent. CJK–CJK justification stays within 0.15em.

Each translated page was compared with the source. Painted advance boxes do not overlap. A prompt example is one block: the label is a single token, the quoted input is translated as a whole, and each line keeps the source x and the sans face. No body line ends on an opening bracket. No monospace listing is painted twice.

## Fixes in this pass

Prompt examples on printed pages 34–50 use one label axis and one content x. A decorative answer quote stays the source width and sits left of the answer. Checked on the v3 export: 0 overlaps between a 22pt quote and a Han glyph, `Tj100` count 0, and every hanging `说明：` / `文本：` / `输出：` / `指令：` ends on the same right edge (span under 2pt) with a full-width colon.

| Defect | Where it showed | Fix |
| --- | --- | --- |
| The 22pt answer quote overlapped the first characters, or a second copy of the mark sat inside the line | p34 `文本应被归类`, p35 `科技` / `历史`, p39 `Java` | A full-width CJK quote is scaled with `Tz` to the source advance (about 10pt) and the scale is reset on its own line. A translation that still opens with the mark drops that character; the source glyph is the only quote. |
| `说明：`, `文本`, and `输出：` sat on different axes, and `文本` had no colon | p34–p50, including title-case `Text` on p39 | Labels share the source small-caps right edge. Every known label, including title-case `Text`, is `文本：` and the body starts at the source content x (117.2 or 126.2), including continuations. |
| `尤利乌斯·凯撒…他在` broke early, and a blank gap sat before the next example | p35 | The lowercase continuation joins the short line, which now runs 126.2–450.4. The following block moves up to the source gap (`现在` ends at y=130.1, the next `说明：` starts at y=143.1). |

## Earlier in this slice

| Defect | Where it showed | Fix |
| --- | --- | --- |
| A moved listing was drawn twice, and an indented continuation stayed on the English baseline | p58–p60 | The redrawn code is removed from the keep set, so the original operators are blanked. Listing lines chain across an indent of up to 16 em, and a prompt arrow on that baseline moves with the code. |
| `使用 LangChain 摘` / `要文本` cut 摘要, and the short line sat on the left | p55 | A display title moves a one- or two-character CJK tail that follows Latin onto the next line. A shared right edge still right-aligns when the English lines differ by only a few points, and the title stops clear of the chapter numeral. |
| The shaded “Beyond Chain of Thought” box stayed English | p47 | A table walk stops after a gap of about four leadings, so the callout under the last cell is translated. The cells of table 2.2 stay English. |
| The running head was `章节 2 以编程方式执行提示` | even pages of chapter 2 | The head is `第2章 以编程方式执行提示词`, the same wording as pages 1–30. |
| A link bullet absorbed the next page’s `Summary` and the mark changed size | p50 | A bullet that is only a URL does not join the next page. Every link in that list keeps the source Wingdings mark at x=108.8, size 8.46. |
| `Thread of Thought` was painted as 思维链 | p48 | The bullet says 思维线程, so it is not the same term as 思维链. |
| Table 2.1 example-column bullets were translated | p40 | Lines under a table caption do not vote for the body face, and later rows use the table's accumulated span. The Rolex and Omega bullets stay English. |
| `2.5 Reasoning in detail` was kept as a table cell | p40 | `2.5` and the title share a baseline but a gutter splits them. That pair stops the table walk. The heading paints as `2.5 详细推理`. |
| Prompt samples split `TEXT` into `T` and `文 本`, mixed the English input with its translation, and stacked lines | p34, p40, p42, p46, p50 | A prompt block is the small-caps label plus the following sans lines. The label is one token (`说明`, `文本`, `指令`, `示例`). The quoted input is translated as a whole. Tags such as `<Tone>` become `<语气>` without breaking inside the brackets. Extra wrapped lines keep at least 1.05em of leading, and a block that grows pushes the next paragraph down. |

## Accepted

- p52 is the chapter verso. The only visible ink besides the running head is the license line.
- p54 is the blank verso before chapter 3. The text layer still has `CHAPTER` with no title. It stays in the source face and is not turned into one CJK character.
- Table 2.1 and table 2.2 cells stay English. The captions are Chinese.
- Monospace prompt samples stay English, including the aqueduct text on p33. Prompt labels set in the sans face are translated (`说明`, `<指令>`).
- The giant `3` on the chapter opener is a path whose box starts near x=367. Both title lines end at x=361, so they do not cross it. `摘要文本` is one word.
- License line is `授权给 THIAGO BANDEIRA <…>` on every page.

## Per page

| Page | Result | What was checked |
| --- | --- | --- |
| 31 | Pass | `2.2 使用 LangChain 运行提示词` stays with the section index. The shell sample under it is English. The following prose is Chinese. |
| 32 | Pass | Running head is `第2章 以编程方式执行提示词`. Body opens in Chinese. The `➥` shell lines stay English and share the code baseline. |
| 33 | Pass | `2.3 提示模板`. The monospace aqueduct sample stays English. The prose around it is Chinese. |
| 34 | Pass | `说明：` and `文本：` share the right edge x=111.2. The headphone sentence and the answer start at x=117.2. The 22pt quote is x=102.2–112.3 and does not touch `文本应被归类`. |
| 35 | Pass | `说明：` / `文本：` / `输出：` share x=120.2. `科技` and `历史` start at x=126.2, clear of the quote at x=111.2–121.3. The Julius line is one run, x=126.2–450.4. The gap after `现在` is about 13pt. |
| 36 | Pass | The sentiment table inside the sample stays English. The `OUTPUT` lead is translated with the sentence. |
| 37 | Pass | The Rolex bullets are one gold square at x=126.2, size 8.46. The words share the hanging indent. |
| 38 | Pass | The sample paragraphs are Chinese. No leftover English body. |
| 39 | Pass | Title-case `Text` paints as `文本：` ending at x=120.1, with `Java` at x=126.2. The answer quote stays at x=111.2 and is not repeated inside `1、4、64、和`. |
| 40 | Pass | Caption `表 2.1 提示词类型及示例` is Chinese. The example column stays English, including the Rolex and Omega bullets. `2.5 详细推理` shares the heading baseline. The one-shot block is `指令：` and `示例：` at the source indent. |
| 41 | Pass | `2.5.2 双样本学习` shares the heading baseline. The revised prompt discussion is Chinese. |
| 42 | Pass | The palindrome example is Chinese. The code fence, where present, stays English. |
| 43 | Pass | ChatGPT’s reply in the sample is Chinese. The page does not open with a leftover English sentence. |
| 44 | Pass | Foo/Bar stays as the label inside the Chinese explanation. The shell prompt mark stays on the code line. |
| 45 | Pass | The `examples = [` JSON block stays English, one copy of each line, including the prompt arrows. |
| 46 | Pass | `2.5.6 思维链` shares the heading baseline. The CoT explanation is Chinese. |
| 47 | Pass | Table 2.2 cells stay English. The shaded box is Chinese: `超越思维链`, then 思维树 and 思维线程, with the arXiv URL intact. |
| 48 | Pass | `(续)` continues the box in Chinese. The ThoT bullet is 思维线程, not 思维链. The list marks in the body lists share x and size within each list. |
| 49 | Pass | `2.6 提示结构`. The structure bullets are one gold square. |
| 50 | Pass | The sample tags are whole tokens: `<指令>`, `<输入>`, `<语气>`, `<输出格式>`. The seven link bullets are the same Wingdings mark at x=108.8, size 8.46. `Summary` is not glued onto the GitHub URL. |
| 51 | Pass | `摘要` is the chapter close. Every summary bullet is the same mark at x=117.8. |
| 52 | Pass | Running head only, plus the license line. |
| 53 | Pass | `第 2 部分` and `摘要` do not overlap. The part introduction is Chinese with a 2em indent. |
| 54 | Pass | Blank verso. `CHAPTER` stays the source label. No lone `第`. |
| 55 | Pass | Title is `使用 LangChain` / `摘要文本`, both ending at x=361, clear of the numeral. The two `本章涵盖` bullets match: x=96.2, size 8.46. `在第1章中` is the opening sentence. |
| 56 | Pass | Running head is `第 3 章 使用 LangChain 对文本进行摘要`. The chapter introduction is Chinese. |
| 57 | Pass | `3.1 总结超出上下文窗口的文档` stays with the index. Figure 3.1’s caption is Chinese. |
| 58 | Pass | The import block and `text_chunks_chain` are one English listing. The `➥chunk_overlap` line and the closing brackets sit on the moved baselines, 10.2 pt apart, with no second copy. |
| 59 | Pass | `3.1.3 映射（Map）`. The `summarize_chunk` listing is English, including the prompt arrow, one copy of each line. The note under it is Chinese. |
| 60 | Pass | `(续)` then `3.1.4 归约`. The reduce-chain listing is English and continuous. The prose around it is Chinese. |
