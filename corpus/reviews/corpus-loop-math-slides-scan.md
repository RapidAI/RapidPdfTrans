# Corpus loop: math paper, slide deck, scanned pamphlet

Three new documents, each a different layout. Translated English to Chinese on current main. Figure interiors, display math, and name lists stay in the source language. Coverage `english_body: 0` and `complete: true` is the pass bar. The PDFs are not committed.

| id | pages translated | PDF |
| --- | --- | --- |
| arxiv-2609.15979, The k-server conjecture is true | 18, letter, Latin Modern | `/opt/cursor/artifacts/pdfs/arxiv-2609.15979-zh.pdf` |
| fosdem-2025-welcome | 36, landscape 720×405 | `/opt/cursor/artifacts/pdfs/fosdem-2025-welcome-zh.pdf` |
| lafollette-illusion-1924 | first 12 of 52, OCR text layer | `/opt/cursor/artifacts/pdfs/lafollette-1924-p1-12-zh.pdf` |

`arxiv-2609.15979` was already in `corpus/manifest.json`. The FOSDEM 2025 deck and the LaFollette OCR text layer were added there (URL, license, sha256). Manning body slices stayed regression-only: `rpt-core` lib tests, including the definition join, the NOTE callout, and the listing annotation, still pass (340 passed, 1 ignored).

## Fixes

| defect | where it showed |
| --- | --- |
| A 36pt title on the next slide continued a 24pt ○ bullet. The size band was measured on the larger face, so 36 vs 24 still counted as one item. | FOSDEM page 2 dinner bullet ran into page 3, “如果需要帮助 / 谁负责什么”. |
| `Your Ideas` is title case, so the byline restore put the English headline back. | Scan page 2. |
| A margin column of `§`, `\|`, `1` was already dropped when the mark stood alone. OCR glues a letter on (`\|\|`, `j§`, `jj\|`) and leaves a stray `S` in the same column, so the sentence was translated with the ornament. A full-width row of `l` from a printed rule was redrawn as visible text. | Scan page 2 ad. |

The slide title now has to stay within 1.2× the list line. A rule column is the repeated x, including a short run glued to a rule character. A stray letter on that column is dropped with the rules. A spaceless line that is mostly `l` and `|` is not a sentence.

## k-server (18 pages)

Body, lemmas, and the appendix proofs are Chinese. Display math, subscripts, and citation keys stay as set. Coverage is complete.

- Pages 1–3. Title and abstract in Chinese. Christian Coester stays Latin; Elias Koutsoupias and Marek Zbysiński are translated because the surname is long or not ASCII. Definitions and the proof sketch read as prose around the formulas.
- Pages 4–13. Theorems and proofs in Chinese. Formulas stay formulas. A few proof labels sit on their own line between the two halves of a wrapped word (page 8, 特别是 split around 证明。). That is a placement leftover, not an English body line.
- Pages 14–17. The bibliography heading is Chinese and some entry titles were translated. Author names and years stay Latin. References are meant to stay English; this section is the remaining gap.
- Page 18. Appendix proofs in Chinese, with the displayed equations kept.

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

- Page 1. Cover OCR is still garbage (洗手间 for the title, a broken byline). Accepted.
- Page 2. The ad paints in Chinese. 您的想法, the Daily Worker lines, the price, and the Chicago address. No `\|`, `§`, `j§`, or `S` in the painted lines. The rule row of `l` is not redrawn.
- Page 3. Title page in Chinese. Jay Lovestone is translated. “Coolidge” stays.
- Page 4. Blank.
- Pages 5–6. Preface in Chinese. One sentence still contains the English word “effects”.
- Page 7–12. Chapters 1 and 2 in Chinese. The heading Business Interests Safe is 商业利益得到保障. “J. P.” style names that are real people stay Latin when the line is a name.

## Left open

- k-server bibliography titles are partly Chinese.
- A proof label can land between the two halves of a wrapped word.
- The scan’s invisible OCR layer is still in the content stream, so a text extract of page 2 shows the old rule characters beside the Chinese.
- Devroom and stand names on the FOSDEM deck stay English on purpose.
