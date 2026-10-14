# Manning body pages 31–60 review

Sample: printed pages 31–60 of *AI Agents and Applications*. That span is PDF pages 59–88 of the full book. The 30-page slice is 531×636.572 pt. Source slice: `/opt/cursor/artifacts/pdfs/manning-body-p31-60-source.pdf`. Translation: `/opt/cursor/artifacts/pdfs/manning-body-p31-60.pdf`.

Pages 1–30, with the listing-gap and column-fill fixes, are `/opt/cursor/artifacts/pdfs/manning-body-p1-30.pdf` (also `manning-body-p1-30-v4.pdf`).

Coverage is complete (`english_body` 0; kept 9949, rewritten 51078, total 61027). Body text is Noto Serif CJK SC Light. Headings are a heavier CJK face and larger than the body. Figure interiors, table cells, and monospace listings stay English. Captions are translated. Paragraphs use a 2em first-line indent.

Each translated page was compared with the source on glyph positions. No CJK word overlaps another. No body line ends on an opening bracket. No monospace listing is painted twice.

## Fixes in this pass

| Defect | Where it showed | Fix |
| --- | --- | --- |
| A moved listing was drawn twice, and an indented continuation stayed on the English baseline | p58–p60 | The redrawn code is removed from the keep set, so the original operators are blanked. Listing lines chain across an indent of up to 16 em, and a prompt arrow on that baseline moves with the code. |
| `使用 LangChain 摘` / `要文本` cut 摘要, and the short line sat on the left | p55 | A display title moves a one- or two-character CJK tail that follows Latin onto the next line. A shared right edge still right-aligns when the English lines differ by only a few points, and the title stops clear of the chapter numeral. |
| The shaded “Beyond Chain of Thought” box stayed English | p47 | A table walk stops after a gap of about four leadings, so the callout under the last cell is translated. The cells of table 2.2 stay English. |
| The running head was `章节 2 以编程方式执行提示` | even pages of chapter 2 | The head is `第2章 以编程方式执行提示词`, the same wording as pages 1–30. |
| A link bullet absorbed the next page’s `Summary` and the mark changed size | p50 | A bullet that is only a URL does not join the next page. Every link in that list keeps the source Wingdings mark at x=108.8, size 8.46. |
| `Thread of Thought` was painted as 思维链 | p48 | The bullet says 思维线程, so it is not the same term as 思维链. |

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
| 34 | Pass | The `PromptTemplate` call stays English, including `num_words=20`. The steps around it are Chinese. |
| 35 | Pass | `2.4 提示类型`. The instruction and text samples are Chinese. Quotation marks keep the source size. |
| 36 | Pass | The sentiment table inside the sample stays English. The `OUTPUT` lead is translated with the sentence. |
| 37 | Pass | The Rolex bullets are one gold square at x=126.2, size 8.46. The words share the hanging indent. |
| 38 | Pass | The sample paragraphs are Chinese. No leftover English body. |
| 39 | Pass | The Java question-and-answer sample is Chinese. `Java` and `JVM` stay Latin inside the sentences. |
| 40 | Pass | Caption `表 2.1 提示词类型及示例` is Chinese. The two columns of examples stay English. |
| 41 | Pass | `2.5.2 双样本学习` shares the heading baseline. The revised prompt discussion is Chinese. |
| 42 | Pass | The palindrome example is Chinese. The code fence, where present, stays English. |
| 43 | Pass | ChatGPT’s reply in the sample is Chinese. The page does not open with a leftover English sentence. |
| 44 | Pass | Foo/Bar stays as the label inside the Chinese explanation. The shell prompt mark stays on the code line. |
| 45 | Pass | The `examples = [` JSON block stays English, one copy of each line, including the prompt arrows. |
| 46 | Pass | `2.5.6 思维链` shares the heading baseline. The CoT explanation is Chinese. |
| 47 | Pass | Table 2.2 cells stay English. The shaded box is Chinese: `超越思维链`, then 思维树 and 思维线程, with the arXiv URL intact. |
| 48 | Pass | `(续)` continues the box in Chinese. The ThoT bullet is 思维线程, not 思维链. The list marks in the body lists share x and size within each list. |
| 49 | Pass | `2.6 提示结构`. The structure bullets are one gold square. |
| 50 | Pass | The seven link bullets are the same Wingdings mark at x=108.8, size 8.46. `Summary` is not glued onto the GitHub URL. |
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
