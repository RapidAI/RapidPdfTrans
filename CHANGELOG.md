# Changelog

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
