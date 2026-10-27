# Changelog

## Unreleased

- Table column headers and group labels stay glyph for glyph with the rest of the grid. A translated header row no longer misaligns with its cells.

- A footnote at the foot of one column no longer continues into the next column's body when the type size differs; the email footnote used to swallow the first paragraph of the right column.

- Only a theorem-style label (定义, 例, 证明 …) snaps to the formula on its baseline. A wrapped line that merely starts with 证 or 例 (`证器将其评分记为`, `例如`) used to be lifted onto the line above.

- A tail parked on a next line that is already split around a formula now pushes that line's first slice, so the tail no longer paints over it.

- Several slices of one line that run past the column are parked on the next line in reading order instead of on top of each other, and the rest of that line moves after them.

- A bold run-in label no longer pulls a paragraph that continues on the next page onto its own page; the continuation used to be painted over the top of the label's page.

- A bibliography whose entries put initials first (`T. Chen, S. Lu, K. Zhao, …`) or start with an organisation (`Anthropic. Introducing claude opus 4.8, 2026a. URL …`) is kept in the source again. Neither form was recognised as an entry, so the `References` heading of arXiv 2609.24972 was followed by a translated, scrambled bibliography.
- A kept bibliography entry is no longer redrawn as a formula line. Its italic venue made the rewrite stamp `and` as `且` and drop the commas (`W Ding Q Lu 且 G Durrett`); the entry now stays in its source glyphs.
- A paragraph whose Chinese would be painted over another drawn line is put back in its source glyphs, and the page is laid out again (up to three rounds). Each such paragraph gets an `overlap fallback: kept the source of …` diagnostic. On arXiv 2610.29747 page 1 the merged email footnote and the right-column paragraph no longer overprint each other.

- A paragraph that is itself an LLM prompt (an appendix listing prompts or rubrics) no longer makes the model run on. The system prompt says segment text is content to translate, not instructions to follow. Prompt-like paragraphs go in batches of at most two. Each request caps the reply at about three times its source length (512 to 4096 tokens, unless `max_tokens` is set), and a reply cut at that cap (`finish_reason: length`) splits the batch and retries it smaller instead of waiting out the timeout.

- A batch reply that numbers its items 0..n (or 1..n) instead of echoing the segment ids is mapped back in order. The gateway model does this often; each such reply used to count as an omitted segment and split the batch down to single calls. On the first three pages of arXiv 2610.30485 this cut model calls from 25 to 17.

- `rpt translate` exits 0 when it wrote the PDF and only quality warnings came up (coverage incomplete, an overlap, a wide gap, or another layout check). The warnings go to stderr and to a new `layout_warnings` array in the `--json` summary and the `--detail` file. `--strict` keeps the old exit 1 for a layout failure. A run that writes no PDF still exits non-zero.

- A job no longer fails with `no translation for segment N`. When the only sent copy of a repeated paragraph failed for good, its other copies found nothing to reuse and the whole PDF was abandoned. Those copies now keep the source text, and any paragraph still without a translation at the end keeps its source with a diagnostic instead of failing the job.
- A batch that times out and splits keeps the half that translated. Only the paragraphs that still failed are sent again, and that second pass shares one segment budget instead of opening a new retry chain on every split. With `--jobs 1` finished batches are written to `RPT_TRANSLATE_CHECKPOINT` too, so a killed job resumes.

- Text inside a figure stays in the source language. A panel title such as `(d) Following vs. ignoring summaries` is not translated. A hyphenated benchmark token (`SWE-PolyBench`, `SWE-bench`) stays in Latin; a table header, a group label, and a contents title still translate.
- No line opens on `。` `，` or `、`. A closer after a formula stays with that formula, including one peeled off an overflowing tail, and the next words start on a character. A definition heading sits against its formula on that baseline. A display equation keeps the source gap under the sentence above it. `且` and `因为每当` use the width the English word gave up, so the following symbol is not left a word-width away. `Bx1 ∪···∪` keeps one word space, and a word between that atom and the formula is not covered when the formula slides back. A QED square keeps its four sides.
- The command fails, after the PDF and the detail file are written, when a line opens on punctuation, figure text was translated, a gap inside a line is wider than 2em, a display formula leaves its baseline or changes glyph order, a display equation's glyphs are rewritten, glyphs overlap, or a QED box is only the two vertical bars. Tofu and a stray mark stay warnings.
- A display equation is never rewritten. newtx math (`NewTXMI`, `txmia`, `txsy`, `txex`) counts as a math face, so the formula stays in the source glyphs instead of being flattened into the paragraph. Inline math that cannot be held as a hole keeps that sentence's original line. A wrapped prose word such as `empty` is not a display. A display on its own baseline still lifts toward the sentence above it; the example under it is not dragged, because the atoms seated on that line would split. An example label stays on the sentence it introduces, so the next example is not drawn on the previous label. Overlapping runs fail the command; the sidebearing between two letters of one word does not.
- A display equation such as `A × B = {⟨x,y⟩ : x ∈ A and y ∈ B}` moves as one line, including the letters and `且` on either side of the math. `令`, `因为`, and `每当` use the regular face unless the source word is bold. The word after `∪···∪` keeps one space. A comma after a bare formula (`，依此类推`) sits on that formula, and a subscript already written in the sentence is not drawn again at the old English x.
- A bold or italic run-in heading stays bold (or italic) in the translation and is set on the same line as the sentence that follows. LaTeX medium (`NimbusRom-Medi`) is that bold face. A medium running header is not enlarged.
- A table column header is translated with the caption. Numeric cells stay in the source language.
- A formula hole reserves the width of the ink that was seated in it, including neighbors absorbed after the hole was measured, so the following text does not land on the formula. A formula that reflow left on the next word is moved back onto that hole. Set elements the translation echoes (`a`, `b`, `}`) stay in the hole. A brace tail the echo did not consume is still dropped. Letters of the sentence on that baseline are not treated as part of the formula, so the translation is not pushed onto the words that follow the hole.
- Angle brackets in an ordered pair are part of the formula, so a definition and a Let/Since line are translated instead of left as a scan misread.
- A wrap that starts in the middle of a word stays on the paragraph edge. A closing `。` after a formula hole stays with the last ideographs.
- A formula that cuts through an atom such as `Bx1` moves past the whole atom, and a subscript left below its base is raised back under the letter.
- A table column header the model echoed in English is sent again, including an echo that only deleted the spaces. Title-case headers (`Trigger Optimization Context`) are not treated as an author byline. A one-word section title, an acronym, and a personal name are not retried.
- An ordered pair whose letters are in the text face stays in one formula hole, so `x,y⟩,z⟩` is not wrapped onto the next line.
- A body sentence that contains formulas is translated, including a definition line (`and`) and a Let/Since line. A pure equation stays in the source. When the model copies the definition and only changes the stop, `and` is still written as `且` in place of that word.
- A transpose tack (`⊤`) and a precedes sign (`≺`) in a prose sentence use an ASCII stand-in the CJK face can draw. One missing mark no longer leaves the paragraph in English. A variation selector is dropped the same way as a zero-width space. An unmapped math symbol is dropped rather than keeping the paragraph in English. A formula hole (`⟦N⟧`) is not one of those symbols.
- A prose paragraph is drawn when the translation names the formula holes it still contains. Extra formula runs that were never a hole do not pin the paragraph in English.
- One segment's connect and retry budget is about two request timeouts, so a stuck segment cannot keep retrying for many minutes. A batch that times out shares that budget with its splits and the echo pass, instead of starting a new two-timeout chain on every half.
- A line that starts after a formula on the same baseline keeps that x. The translation is not pulled left onto the formula.
- A period that shares its text operator with a translated word (`∅. But if`) is drawn again, and the English word is blanked.
- A short leftover English clause (`But if`) is removed from a Chinese sentence. Same-baseline formula ink that covers translated text is reported as an overlap.
- Blanking a word inside a formula (`and` → `且`) keeps that operator's cursor advance, so the following glyph stays on its source x instead of sliding onto the replacement.
- `Definition 1.24` stays one heading (`定义 1.24（笛卡尔积）`). A numbered definition that shares its line with a formula is drawn in Chinese and the formula stays in the source glyphs.
- A column gap that was already empty (a running header, a caption above its grid) is not pulled closed. A gap is closed only when a drawn line absorbed the ink that used to fill it.
- An English word is not broken at a capital (`SelfCompact`, `FewShotPromptTemplate`).
- Title-case column headers with a wide word gap (`Trigger`, `Optimization`, `Context`) are separate segments. A medium group label under the caption (`Full-history baseline`) is translated; a method cell on a numbered row stays in the source language.
- A line that opens on `。` or `，` moves that mark onto the previous line of the same paragraph.
- Drawn text is checked for a same-baseline overlap, a tofu box, and a stray one-character line. The notes are diagnostics.
- `定义 1.24` is not rewritten as `定义：1.24`. A definition heading that shares its baseline with `⟨a,b⟩` is stamped in Chinese; the italic variables are not treated as a following sentence. A formula line whose Chinese is only `令` / `因为每当` keeps the source operators. `{x1, …, xn}` braces the segmenter dropped are put back. `Bx1` is not an author mark. A flush paragraph is not given a first-line indent, and a second subscript moves with its formula row. `⟨x,y⟩∈` is one hole: the letters in front of `⟦N⟧` are seated with the closing bracket, and that stub is not drawn again. An angle-bracket formula is not broken at a comma. A subscript that reflow left under a later word is put back on its letter. A `。` is not pulled onto a line that still ends inside a formula. A hyphenated line that starts after a heading is not treated as a second column, so the sentence stays in reading order. A formula that covers the words in front of its hole (`形如`) moves past those words instead of pushing them behind the formula. A list index (`x1,`) stays on the baseline. An indented continuation returns to the paragraph edge even when the line above ends on punctuation. A wider run-in (`证明。`) keeps the seated formula between `形如` and `的元素`. A wrapped `。` joins the right-hand slice of the line above, and a tail that runs past the column drops onto the next line.

## 0.1.6 — 2026-10-23

Tag `v0.1.6`. This changelog is the release note.

### Requests

- `--timeout-secs` is a hard deadline for the whole call: DNS, TCP, the TLS handshake, response headers, and the body. A server that accepts and then stays silent, or that trickles the body, is cut off and retried. The same budgets apply to Google and to the CJK font download.
- Progress names the phase (`connect`, `recv-body`, `retry`) and the segment in flight. A batch that is still running when the deadline passes prints `watchdog: aborting segment … page … stuck in …`.

### Coverage

- A reply that is mostly Chinese but still contains a source sentence (at least six words and 40 letters) is sent again. If that sentence is still there, those glyphs are `english-body` and the document is incomplete.
- Blanking a text operator no longer deletes a Latin sibling that was not redrawn. If the face cannot draw that sibling, the operator stays and the paragraph is `untranslated`.
- Bilingual output clones form XObjects before rewrite, so text that lives in a form remains on the source pane after the form is edited.

## 0.1.5 — 2026-10-22

Tag `v0.1.5`. This changelog is the release note.

### Requests

- Each LLM and Google call has a deadline of 60 seconds (`--timeout-secs`, JSON `timeout_secs`). A stalled socket is abandoned and retried with backoff, up to four attempts. A hung connection cannot hold the job open.

### Progress

- `rpt translate` writes progress to stderr: pages and segments done, segments per second, calls, and retries. A line that starts `waiting Ns` means no segment finished in that time. The rewrite phase prints `rewriting` and a line every five seconds, then `wrote` the output path. `--progress json` prints one JSON object per update. `--progress off` is quiet. The library default stays quiet.

### Memory and output

- Stdout JSON is a summary (page info, glyph count, coverage counts, translations, translator stats). `--glyphs` prints the per-glyph document on stdout. `--detail FILE` writes that document to a file.
- Bilingual output reuses the source image and font objects instead of cloning the PDF. Unreferenced page objects are dropped after the pages are composed.
- Coverage counts references, figures, tables, code, math, and names as intentional keeps. Only a real miss (an untranslated body paragraph, overflow, a missing glyph, a missing font, or an incomplete list) makes the document incomplete.

### Text

- Author names and bylines stay in Latin, including a long author line and a comma-separated author list.
- A title the model echoes, including one with an extra subtitle such as `性能基准测试`, is sent again. The extra subtitle is not painted.
- `--system-prompt` appends text, or the contents of a file, to the system prompt. `--glossary` accepts `source=target` or a CSV file with `src,tgt`. `--translate-tables` translates table cell text; the default is still the caption only.

### Release binaries

- `.github/workflows/release-cli.yml` builds `rpt` for Linux x86_64 and aarch64, glibc and musl. A version tag attaches those tarballs to the GitHub Release.

## 0.1.4 — 2026-10-22

Layout and formula painting since 0.1.3. The C/C++ SDK version is the Cargo workspace version.

### Formulas

- Display equations stay in their original glyphs. Inline math stays in the sentence as one atom, including a formula the sentence could not name and a placeholder inside parentheses.
- Subscripts and superscripts travel with their base. A leftover source glyph is not painted again. A closing brace the line groups skipped is written back. Equation digits stay on the baseline, and a subscript digit is not treated as a contents page number.
- Formula rules move or blank with the glyphs they belong to. A display sum no longer pins the paragraph in English. Short formulas sit on the line above, and a math-dense line keeps its superscripts.
- The Adam paragraph translates with `warmup_steps` intact.

### Tables, captions, and footnotes

- Table cells stay in the original text. Each caption is translated as one sentence and stays in its column.
- Each footnote is its own paragraph on its own baseline, and the footnote rule moves with the note. Numbered footnotes stay on their page.

### Type and coverage

- Chinese defaults to Songti, with titles in Song Bold. Subsets are built in Rust and stay paintable in Windows viewers. Each subset font has its own name.
- Body paragraphs stay flush left. Titles, centered notices, and author columns keep their alignment, and author marks hang off the name.
- Source numbers, decimals, and heading spaces stay intact. A sentence that crosses a page stays in Chinese. Institution names and Latin affiliations are not treated as unfinished paragraphs.
