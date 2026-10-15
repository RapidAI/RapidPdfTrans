# Manning body pages 31–60 review

Sample: printed pages 31–60 of *AI Agents and Applications*. That span is PDF pages 59–88 of the full book. The 30-page slice is 531×636.572 pt. Source slice: `/opt/cursor/artifacts/pdfs/manning-body-p31-60-source.pdf`. Translation: `/opt/cursor/artifacts/pdfs/manning-body-p31-60.pdf`.

Pages 1–30 are `/opt/cursor/artifacts/pdfs/manning-body-p1-30.pdf` (also `manning-body-p1-30-v10.pdf`). This slice is `manning-body-p31-60-v7.pdf`.

Coverage is complete (`english_body` 0; kept 10090, rewritten 50937, total 61027). Body text is Noto Serif CJK SC Light. Headings are a heavier CJK face and larger than the body. Figure interiors, table cells, and monospace listings stay English. Captions are translated. Paragraphs use a 2em first-line indent. CJK–CJK justification stays within 0.15em.

Each translated page was compared with the source. Painted advance boxes do not overlap. A prompt example is one block: the label is a single token, the quoted input is translated as a whole, and each line keeps the source x and the sans face. No body line ends on an opening bracket. No monospace listing is painted twice.

## Fixes in this pass

A `NOTE` whose translation began `请注意，…` is the same callout as `定义：`. The label is `注意：` in the source label colour at the source label x, and the sentence starts at the source body x. Later lines sit on the callout’s left edge. A callout line is justified only when the extra space stays inside 0.15em; a line that still would not reach the margin is left tight.

Printed page 57 is the case that showed the bug. `注意：` is at x=114.2. `在完整的《白鲸》文本上运行代码可能会比较昂贵。提供的` starts at x=144.9, the same x as source `Keep`, and is one run through x=386.0 with `Moby-Dick.txt` after it. The next line is at x=114.2. The same split holds for every `NOTE`, `DEFINITION`, and `TIP` in both slices: the label x matches the source label, and the first body word matches the source body x (pages 31–60: 1, 8, 13, 17, 20, 26, 27, 28, 29; pages 1–30: 5, 6, 17, 21, 22, 29, 30).

A sentence that crosses a page is one source paragraph, translated as one unit, then placed back onto each page’s lines. The next page keeps that paragraph’s colour and starts at the source continuation x, with no extra first-line indent. A line does not cut `词元`, `一个词`, `处理`, `法律`, `所有`, or `编排`.

The definition on printed pages 56–57 was the case that showed the bug. `定义：` is blue at x=105.2. The first body line starts at x=159.4 in gray. Later lines on page 56 sit at x=105.2. Page 57 continues at x=114.2 with `处理超过 1 百万个 token。` in the same gray. The next paragraph’s first line is the indented one, at x=120.1. `一个词元` and `一个词` each stay on one line. The page break is `可以` / `处理`, not `处` / `理`.

Cross-page paragraphs in this slice (printed pages): 39–40 (`as I’ll` / `explain`), 42–43 (the Abra classification), 43–44 (`NOTE`), 55–56 (summarization engines), 56–57 (`DEFINITION`), 58–59 (`NOTE` on LangChain chains). A gutter index such as `3.1.1` or `2.5` no longer hides the continuation, and a hyphenated right column still finishes in the next page’s left column.

## Previous pass

Hanging prompt labels match the source: no colon, one shared right edge, a slightly smaller sans (0.85 of the sample size, box height 7.25 against a body of 8.53), and the sample text at the source content x. Checked on the v4 export: `Tj100` count 0, 0 overlaps between a quote taller than 12 pt and another word, and every hanging label on a page ends on the same right edge (span 0).

| Defect | Where it showed | Fix |
| --- | --- | --- |
| Labels were painted with a full-width colon, and `OUTPUT FORMAT` split at the space so the body started under `FORMAT` | p34–p50, p49 `输出格式` | The painted label is the stem only (`说明`, `文本`, `输出`, `指令`, `角色设定`, `背景`, `输入`, `语气`, `输出格式`). The longest known label is taken, so `OUTPUT FORMAT` stays one label through `FORMAT`. On p49 that label ends at x=120.2 and `请以两段形式呈现文本` starts at x=126.2. |
| `Text` in the third sentiment example was `文字` | p35 green dress | `TEXT` is always `文本`. `说明` and `指令` stay as the model wrote them. A content-line `Examples:` stays `示例：` at the content x (p46, x=117.2). Callouts stay `定义：` / `注意：`. |
| `Stock 2` and `Stock 3` shared a line, and the heads mixed `股票 2` with `股票 3:` | p35 | Each `股票N：` starts its own line at the content x. The three reports sit at y=508.8, 522.2, and 535.6, all x=126.2, and the digits stay on the item baseline. |
| `股票2：英伟达：销售额正受到…推动` was letter-spaced | p35 | The last or only line of an item, and a prompt line that does not fill the measure, is not justified. `股票2：` is one run, x=126.2–352.7. |

## Previous pass

A decorative answer quote stays the source width and sits left of the answer. The Julius line is one run, x=126.2–450.4. `现在` ends at y=130.1 and the next `说明` starts at y=144.1.

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
| 34 | Pass | `说明` and `文本` share the right edge x=111.2, box height 7.25, no colon. The headphone sentence and the answer start at x=117.2. The quote is x=102.2–112.3 and does not touch `文本应被归类`. |
| 35 | Pass | `说明` / `文本` / `输出` / `指令` share x=120.2. `科技`, `历史`, `中立`, and `顾客买了一条绿色的裙子` start at x=126.2, clear of the quote at x=111.2–121.3. The third example’s label is `文本`, not `文字`. The Julius line is one run, x=126.2–450.4. `股票1：`, `股票2：`, and `股票3：` are three lines at x=126.2. `股票2：` is not letter-spaced (x=126.2–352.7). |
| 36 | Pass | The sentiment table inside the sample stays English. The `OUTPUT` lead is translated with the sentence. |
| 37 | Pass | The Rolex bullets are one gold square at x=126.2, size 8.46. The words share the hanging indent. |
| 38 | Pass | The sample paragraphs are Chinese. No leftover English body. |
| 39 | Pass | Title-case `Text` paints as `文本` ending at x=120.1, with `Java` at x=126.2. The answer quote stays at x=111.2 and is not repeated inside `1、4、64、和`. |
| 40 | Pass | Caption `表 2.1 提示词类型及示例` is Chinese. The example column stays English, including the Rolex and Omega bullets. `2.5 详细推理` shares the heading baseline. The one-shot block is hanging `指令` and `示例` (no colon) ending at x=111.1, body at x=117.2. |
| 41 | Pass | `2.5.2 双样本学习` shares the heading baseline. The revised prompt discussion is Chinese. |
| 42 | Pass | The palindrome example is Chinese. The code fence, where present, stays English. |
| 43 | Pass | ChatGPT’s reply in the sample is Chinese. The page does not open with a leftover English sentence. |
| 44 | Pass | Foo/Bar stays as the label inside the Chinese explanation. The shell prompt mark stays on the code line. |
| 45 | Pass | The `examples = [` JSON block stays English, one copy of each line, including the prompt arrows. |
| 46 | Pass | `2.5.6 思维链` shares the heading baseline. The CoT explanation is Chinese. |
| 47 | Pass | Table 2.2 cells stay English. The shaded box is Chinese: `超越思维链`, then 思维树 and 思维线程, with the arXiv URL intact. |
| 48 | Pass | `(续)` continues the box in Chinese. The ThoT bullet is 思维线程, not 思维链. The list marks in the body lists share x and size within each list. |
| 49 | Pass | `2.6 提示结构`. `角色设定`, `背景`, `输入`, `语气`, and `输出格式` share the right edge x=120.2 (height 7.25, no colon). `请以两段形式呈现文本，每段均为5行。` starts at x=126.2. The structure bullets are one gold square. |
| 50 | Pass | The sample tags are whole tokens: `<指令>`, `<输入>`, `<语气>`, `<输出格式>`. The seven link bullets are the same Wingdings mark at x=108.8, size 8.46. `Summary` is not glued onto the GitHub URL. |
| 51 | Pass | `摘要` is the chapter close. Every summary bullet is the same mark at x=117.8. |
| 52 | Pass | Running head only, plus the license line. |
| 53 | Pass | `第 2 部分` and `摘要` do not overlap. The part introduction is Chinese with a 2em indent. |
| 54 | Pass | Blank verso. `CHAPTER` stays the source label. No lone `第`. |
| 55 | Pass | Title is `使用 LangChain` / `摘要文本`, both ending at x=361, clear of the numeral. The two `本章涵盖` bullets match: x=96.2, size 8.46. `在第1章中` is the opening sentence. |
| 56 | Pass | Running head is `第 3 章 使用 LangChain 对文本进行摘要`. `定义：` is blue at x=105.2. The gray body starts at x=159.4 and wraps at x=105.2. `一个词元` and `一个词` are each intact. The page ends on `可以`. |
| 57 | Pass | `3.1 总结超出上下文窗口的文档` stays with the index. The definition continues at x=114.2 in the same gray, `处理超过 1 百万个 token。`, with no first-line indent. The next paragraph indents to x=120.1. The note at the bottom is `注意：` at x=114.2 and `在完整的《白鲸》…` at x=144.9, with no letter-spacing. Figure 3.1’s caption is Chinese. |
| 58 | Pass | The import block and `text_chunks_chain` are one English listing. The `➥chunk_overlap` line and the closing brackets sit on the moved baselines, 10.2 pt apart, with no second copy. |
| 59 | Pass | `3.1.3 映射（Map）`. The `summarize_chunk` listing is English, including the prompt arrow, one copy of each line. The note under it is Chinese. |
| 60 | Pass | `(续)` then `3.1.4 归约`. The reduce-chain listing is English and continuous. The prose around it is Chinese. |
