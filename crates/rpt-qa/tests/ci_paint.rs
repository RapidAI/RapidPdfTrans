//! A rewritten CI page must paint the translation, not merely expose it through
//! ToUnicode. Drop rate and style retention stay high when the embedded CFF
//! font is an OpenType wrapper, so those scores are not the check.

use std::path::{Path, PathBuf};
use std::process::Command;

use std::collections::HashSet;

use rpt_core::{
    translate_extraction, Disposition, ExtractOptions, PdfDocument, RewriteOptions,
    TranslateOptions, Translator,
};
use rpt_qa::score::{score_pair, ScoreOptions};
use serde_json::{json, Value};

struct ProseToChinese;

impl Translator for ProseToChinese {
    fn complete(&self, _system: &str, user: &str) -> rpt_core::Result<String> {
        let payload: Value = serde_json::from_str(user).unwrap_or(json!({}));
        let segments = payload["segments"].as_array().cloned().unwrap_or_default();
        let translations: Vec<Value> = segments
            .iter()
            .map(|segment| {
                let text = segment["text"].as_str().unwrap_or("");
                let letters = text.chars().filter(|ch| ch.is_ascii_alphabetic()).count();
                let stops = text.matches(". ").count();
                let translated = if text.matches(',').count() >= 3
                    && letters >= 8
                    && stops < 2
                    && !text.contains('@')
                {
                    // A topic list must keep its names. A Han prefix marks the
                    // line translated without dropping those tokens.
                    let mut body = format!("中文回归 {text}");
                    body.push_str(&placeholders(text));
                    body
                } else if letters >= 40 {
                    // Chinese is about twice as wide as the English it replaces.
                    // A one-line source stays one line, so the stand-in is half
                    // as many characters and still contains 中文回归.
                    let mut body = "中文回归".repeat((text.chars().count() / 8).max(5));
                    body.push_str(&placeholders(text));
                    body
                } else {
                    text.to_string()
                };
                json!({"id": segment["id"], "text": translated})
            })
            .collect();
        Ok(json!({"translations": translations}).to_string())
    }
}

#[test]
fn ci_papers_paint_the_chinese_inside_the_source_block() {
    let font = "/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc";
    assert!(
        Path::new(font).is_file(),
        "fonts-noto-cjk is required so this regression cannot skip"
    );
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/ci");
    let mut pdfs: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("pdf"))
        .collect();
    pdfs.sort();
    assert!(
        pdfs.len() >= 5,
        "expected the CI paper set in {}",
        root.display()
    );
    for pdf in &pdfs {
        paint_one(pdf);
    }
}

fn paint_one(pdf: &Path) {
    if let Ok(only) = std::env::var("RPT_ONLY") {
        if !pdf.to_string_lossy().contains(&only) {
            return;
        }
    }
    let mut doc = PdfDocument::open(pdf).unwrap_or_else(|err| panic!("{}: {err}", pdf.display()));
    let mut extraction = doc.extract_with(&ExtractOptions {
        max_pages: Some(1),
        ..ExtractOptions::default()
    });
    let report = translate_extraction(
        &mut extraction,
        &TranslateOptions::default(),
        &ProseToChinese,
    )
    .unwrap_or_else(|err| panic!("{}: {err}", pdf.display()));
    doc.rewrite(&mut extraction, &report, &RewriteOptions::default())
        .unwrap_or_else(|err| panic!("{}: {err}", pdf.display()));
    extraction
        .assert_complete()
        .unwrap_or_else(|err| panic!("{}: {err}", pdf.display()));
    let rewritten: HashSet<u32> = extraction
        .glyphs
        .iter()
        .filter_map(|glyph| {
            matches!(glyph.disposition, Disposition::Rewritten { .. }).then_some(glyph.id)
        })
        .collect();
    let expected: Vec<String> = report
        .segments
        .iter()
        .filter(|segment| {
            segment.translated.contains('中')
                && segment.glyph_ids.iter().any(|id| rewritten.contains(id))
        })
        .map(|segment| segment.translated.clone())
        .collect();
    assert!(
        !expected.is_empty(),
        "{}: no prose segment was rewritten",
        pdf.display()
    );
    let out = std::env::temp_dir().join(format!(
        "rpt-paint-{}.pdf",
        pdf.file_stem().unwrap().to_string_lossy()
    ));
    let bytes = doc.save_bytes().unwrap();
    std::fs::write(&out, &bytes).unwrap();

    let text = Command::new("pdftotext")
        .args(["-enc", "UTF-8", "-q", "-f", "1", "-l", "1"])
        .arg(&out)
        .arg("-")
        .output()
        .unwrap();
    assert!(text.status.success(), "{}", pdf.display());
    let extracted = String::from_utf8_lossy(&text.stdout);
    let squashed: String = extracted.chars().filter(|ch| !ch.is_whitespace()).collect();
    assert!(
        squashed.contains("中文回归"),
        "{}: pdftotext has no translation:\n{extracted}",
        pdf.display()
    );
    let cjk = squashed.chars().filter(|ch| is_cjk(*ch)).count();
    assert!(cjk >= 20, "{}: pdftotext CJK count is {cjk}", pdf.display());
    for segment in expected.iter().take(8) {
        let needle: String = segment.chars().filter(|ch| is_cjk(*ch)).take(8).collect();
        // Repair can splice a source word such as 部分 into the stand-in.
        // Only the pure 中文回归 runs have to show up unchanged.
        if needle.chars().any(|ch| !"中文回归".contains(ch)) {
            continue;
        }
        assert!(
            squashed.contains(&needle),
            "{}: missing segment `{needle}`",
            pdf.display()
        );
    }

    let score = score_pair(
        pdf,
        &out,
        &ScoreOptions {
            render_pages: 0,
            max_pages: Some(1),
        },
    )
    .unwrap();
    assert!(
        score.paint_font_ok,
        "{}: translation font is an OpenType wrapper",
        pdf.display()
    );
    assert!(
        score.horizontal_containment > 0.9,
        "{}: translated blocks leave the source line (containment {})",
        pdf.display(),
        score.horizontal_containment
    );
    assert!(
        score.cjk_extract_ratio > 0.9,
        "{}: pdftotext dropped CJK ({})",
        pdf.display(),
        score.cjk_extract_ratio
    );
    assert!(
        score.overflow_rate < 0.02,
        "{}: glyphs leave the page ({})",
        pdf.display(),
        score.overflow_rate
    );

    let dir = std::env::temp_dir().join(format!(
        "rpt-paint-png-{}",
        pdf.file_stem().unwrap().to_string_lossy()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let status = Command::new("pdftoppm")
        .args(["-png", "-r", "160", "-f", "1", "-l", "1"])
        .arg(&out)
        .arg(dir.join("page"))
        .status()
        .unwrap();
    assert!(status.success(), "{}", pdf.display());
    let png = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| path.extension().and_then(|ext| ext.to_str()) == Some("png"))
        .unwrap_or_else(|| panic!("{}: pdftoppm wrote no png", pdf.display()));
    let again = PdfDocument::open_bytes(&bytes)
        .unwrap()
        .extract_with(&ExtractOptions {
            max_pages: Some(1),
            ..ExtractOptions::default()
        });
    let (x, baseline, size) = glyph_run(&again, "中文回归")
        .unwrap_or_else(|| panic!("{}: re-extract has no 中文回归 run", pdf.display()));
    let page_h = again.pages[0].media_box[3] - again.pages[0].media_box[1];
    let rendered = image::open(&png).unwrap().to_rgb8();
    let scale = rendered.height() as f32 / page_h;
    let left = ((x - size * 0.3) * scale).max(0.0) as u32;
    let top = ((page_h - (baseline + size * 1.05)) * scale).max(0.0) as u32;
    let right = ((x + size * 4.6) * scale).min(rendered.width() as f32) as u32;
    let bottom = ((page_h - (baseline - size * 0.35)) * scale).min(rendered.height() as f32) as u32;
    assert!(right > left + 4 && bottom > top + 4, "{}", pdf.display());
    let crop =
        image::imageops::crop_imm(&rendered, left, top, right - left, bottom - top).to_image();
    let crop_path = dir.join("crop.png");
    crop.save(&crop_path).unwrap();
    let ocr = Command::new("tesseract")
        .args([
            crop_path.to_str().unwrap(),
            "stdout",
            "-l",
            "chi_sim",
            "--psm",
            "7",
        ])
        .output()
        .unwrap();
    let seen = String::from_utf8_lossy(&ocr.stdout);
    let seen_cjk: String = seen.chars().filter(|ch| is_cjk(*ch)).collect();
    // chi_sim often reads 中 as 诗. Two of the four painted characters are
    // enough to show the crop is ink, not a ToUnicode-only rewrite.
    let hits = ["中", "文", "回", "归"]
        .into_iter()
        .filter(|ch| seen_cjk.contains(ch))
        .count();
    assert!(
        hits >= 2,
        "{}: rendered line is not 中文回归 (OCR of the glyph crop, not ToUnicode): {seen}",
        pdf.display()
    );
}

fn glyph_run(extraction: &rpt_core::Extraction, text: &str) -> Option<(f32, f32, f32)> {
    let chars: Vec<char> = text.chars().collect();
    let mut glyphs: Vec<_> = extraction
        .glyphs
        .iter()
        .filter(|glyph| !glyph.unicode.is_empty())
        .collect();
    glyphs.sort_by(|a, b| {
        a.page_index
            .cmp(&b.page_index)
            .then(b.matrix[5].total_cmp(&a.matrix[5]))
            .then(a.matrix[4].total_cmp(&b.matrix[4]))
    });
    for window in glyphs.windows(chars.len()) {
        let matched = window.iter().zip(&chars).all(|(glyph, ch)| {
            glyph.unicode.starts_with(*ch)
                && glyph.page_index == window[0].page_index
                && (glyph.matrix[5] - window[0].matrix[5]).abs() < 0.8
        });
        if matched {
            let glyph = window[0];
            return Some((glyph.matrix[4], glyph.matrix[5], glyph.font_size));
        }
    }
    None
}

fn placeholders(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('⟦') {
        let Some(end) = rest[start..].find('⟧') else {
            break;
        };
        let token_end = start + end + '⟧'.len_utf8();
        out.push_str(&rest[start..token_end]);
        rest = &rest[token_end..];
    }
    out
}

fn is_cjk(ch: char) -> bool {
    ('\u{3400}'..='\u{9FFF}').contains(&ch)
}
