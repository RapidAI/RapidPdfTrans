# CI PDFs

Nine real papers committed so GitHub Actions can run the fidelity harness without downloading the rest of the corpus. Every file is CC BY 4.0 (`https://creativecommons.org/licenses/by/4.0/`). Each is under 700 KB. The nine files together are about 4.1 MB.

`manifest.json` records the direct download URL, source, license, sha256, size, page count, and feature tags. The PDFs sit next to that manifest (`{id}.pdf`). CI runs:

```bash
cargo run -p rpt-qa -- --manifest corpus/ci/manifest.json --cache corpus/ci --render-pages 1
```

Everything else stays in `../manifest.json` and is fetched into the gitignored `../cache/` directory.

| File | Bytes | What it adds |
| --- | ---: | --- |
| `arxiv-2610.02163.pdf` | 285600 | Single-column CC BY arXiv paper. Formulas, a code listing, tables, figures, Form XObject text, Identity-H, custom Type 1 encodings, link annotations. |
| `pmlr-v202-abbas23a.pdf` | 329081 | Two-column ICML 2023 paper (PMLR, CC BY). Formulas, footnotes, tables, figures, bold, italic, and non-black text. |
| `neurips-2023-00296c0e.pdf` | 340260 | NeurIPS 2023 conference paper (CC BY). Single-column in this file, formulas, figures, footnotes, link annotations. |
| `arxiv-2610.01998.pdf` | 370206 | Math paper (CC BY). Type 3 font `F84`, Computer Modern builtin encodings, a little Form XObject text, Identity-H. |
| `pmlr-v202-abels23a.pdf` | 504216 | Two-column ICML paper (CC BY). Many Type 3 fonts, Form XObject text, Identity-H. |
| `neurips-2017-attention.pdf` | 569417 | NeurIPS 2017 “Attention Is All You Need” (CC BY). Formulas, tables, figures. Type 1 faces with no ToUnicode CMap (custom and builtin encodings). |
| `2024-acl-long-7.pdf` | 614450 | ACL 2024 paper (CC BY). Two-column. Almost all text is inside Form XObjects. Type 3 DejaVu fonts, code, tables, figures, footnotes. |
| `arxiv-2610.02193.pdf` | 617457 | CC BY arXiv paper. Type 3 fonts, Form XObject text, ActualText, Identity-H CID TrueType (Helvetica and Times). |
| `arxiv-2609.36965.pdf` | 662639 | CC BY arXiv paper with Chinese text (ChillRoundF, Identity-H). Two-column, formulas, code, unembedded Standard 14 fonts, Form XObject text. |

Link annotations are hyperlink dictionaries. Their appearances did not yield extra text glyphs on the pages the harness extracts. Type 3 coverage is the font and its char proc being present; many of those procs draw marks rather than nested text.
