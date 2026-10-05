//! Fidelity checks for corpus PDFs.
//!
//! Until content-stream rewrite exists, the identity path copies the original
//! bytes and checks that extraction, style, and (when Poppler is installed)
//! rendered pages match. A separate lopdf save measures how much a structural
//! rewrite perturbs glyphs. It is not a visual rewrite of translated text.

use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use rpt_core::{ExtractOptions, Extraction, PdfDocument};
use serde::{Deserialize, Serialize};

use rpt_qa::check::{align, observed_tags, poppler_cross_check, AlignReport, TextCrossCheck};
use rpt_qa::render::{compare_renders, RenderReport};

#[derive(Parser)]
#[command(
    name = "rpt-qa",
    about = "Extraction and fidelity report for the PDF corpus"
)]
struct Cli {
    #[arg(long, default_value = "corpus/manifest.json")]
    manifest: PathBuf,
    #[arg(long, default_value = "corpus/cache")]
    cache: PathBuf,
    #[arg(long, default_value = "corpus/reports")]
    out: PathBuf,
    /// Only documents marked `"ci": true`.
    #[arg(long)]
    ci: bool,
    /// Pages to rasterize per document.
    #[arg(long, default_value_t = 2)]
    render_pages: u32,
    /// Documents longer than this are extracted only up to `page_cap`.
    #[arg(long, default_value_t = 40)]
    full_extract_until: usize,
    #[arg(long, default_value_t = 12)]
    page_cap: u32,
}

#[derive(Clone, Debug, Deserialize)]
struct Manifest {
    documents: Vec<Document>,
}

#[derive(Clone, Debug, Deserialize)]
struct Document {
    id: String,
    url: String,
    source: String,
    title: String,
    license: String,
    sha256: String,
    bytes: u64,
    pages: Option<u32>,
    tags: Vec<String>,
    ci: bool,
}

#[derive(Clone, Debug, Serialize)]
struct FileReport {
    id: String,
    url: String,
    source: String,
    title: String,
    license: String,
    tags: Vec<String>,
    observed_tags: Vec<String>,
    bytes: u64,
    pdf_pages: Option<usize>,
    extracted_pages: usize,
    truncated: bool,
    status: String,
    error_class: Option<String>,
    error: Option<String>,
    glyphs: usize,
    unmapped: usize,
    unmapped_ratio: f32,
    /// Base font names of glyphs with no Unicode mapping, most frequent first.
    unmapped_fonts: Vec<Count>,
    diagnostics: usize,
    diagnostic_samples: Vec<String>,
    coverage_complete: bool,
    pending: usize,
    identity: Option<AlignReport>,
    lopdf_rewrite: Option<AlignReport>,
    lopdf_error: Option<String>,
    text_cross_check: Option<TextCrossCheck>,
    render: Option<RenderReport>,
}

#[derive(Clone, Debug, Serialize)]
struct Summary {
    documents: usize,
    extracted: usize,
    open_failures: usize,
    panics: usize,
    glyphs: usize,
    unmapped: usize,
    identity_text_matches: usize,
    identity_style_clean: usize,
    lopdf_text_matches: usize,
    lopdf_attempted: usize,
    lopdf_failures: usize,
    lopdf_skipped: usize,
    mean_ours_in_poppler: Option<f32>,
    mean_poppler_in_ours: Option<f32>,
    mean_identity_ssim: Option<f32>,
    truncated: usize,
    by_source: Vec<Count>,
    by_tag: Vec<Count>,
    by_observed_tag: Vec<Count>,
    failures: Vec<Failure>,
}

#[derive(Clone, Debug, Serialize)]
struct Count {
    name: String,
    count: usize,
}

#[derive(Clone, Debug, Serialize)]
struct Failure {
    id: String,
    class: String,
    message: String,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("rpt-qa: {err}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: &Cli) -> Result<(), String> {
    let manifest: Manifest = serde_json::from_str(
        &fs::read_to_string(&cli.manifest).map_err(|err| format!("read manifest: {err}"))?,
    )
    .map_err(|err| format!("parse manifest: {err}"))?;
    let mut docs: Vec<Document> = manifest.documents;
    if cli.ci {
        docs.retain(|doc| doc.ci);
    }
    if docs.is_empty() {
        return Err("no documents selected".into());
    }
    fs::create_dir_all(&cli.out).map_err(|err| err.to_string())?;
    let mut reports = Vec::new();
    for doc in &docs {
        let path = cli.cache.join(format!("{}.pdf", doc.id));
        eprintln!("qa {}", doc.id);
        reports.push(analyze_document(doc, &path, cli));
    }
    let summary = summarize(&docs, &reports);
    let payload = serde_json::json!({
        "summary": summary,
        "files": reports,
    });
    let json_path = cli
        .out
        .join(if cli.ci { "ci.json" } else { "summary.json" });
    let md_path = cli.out.join(if cli.ci { "ci.md" } else { "summary.md" });
    fs::write(
        &json_path,
        serde_json::to_string_pretty(&payload).map_err(|err| err.to_string())?,
    )
    .map_err(|err| err.to_string())?;
    fs::write(&md_path, markdown(&summary, &reports)).map_err(|err| err.to_string())?;
    eprintln!("wrote {} and {}", json_path.display(), md_path.display());
    Ok(())
}

fn analyze_document(doc: &Document, path: &Path, cli: &Cli) -> FileReport {
    let mut report = empty_report(doc);
    if !path.exists() {
        report.status = "missing".into();
        report.error_class = Some("missing".into());
        report.error = Some(format!("not in cache: {}", path.display()));
        return report;
    }
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            report.status = "error".into();
            report.error_class = Some("io".into());
            report.error = Some(err.to_string());
            return report;
        }
    };
    report.bytes = bytes.len() as u64;
    if !doc.sha256.is_empty() {
        let digest = sha256(&bytes);
        if digest != doc.sha256 {
            report.status = "error".into();
            report.error_class = Some("checksum".into());
            report.error = Some("sha256 does not match the manifest".into());
            return report;
        }
    }
    let opened = catch_unwind(AssertUnwindSafe(|| PdfDocument::open_bytes(&bytes)));
    let document = match opened {
        Ok(Ok(document)) => document,
        Ok(Err(err)) => {
            let message = err.to_string();
            report.status = "error".into();
            report.error_class = Some(classify(&message).into());
            report.error = Some(trim(&message));
            return report;
        }
        Err(panic) => {
            report.status = "panic".into();
            report.error_class = Some("panic".into());
            report.error = Some(trim(&panic_message(panic)));
            return report;
        }
    };
    let pdf_pages = document.page_count();
    report.pdf_pages = Some(pdf_pages);
    let cap = if pdf_pages > cli.full_extract_until {
        report.truncated = true;
        Some(cli.page_cap)
    } else {
        None
    };
    let extraction = match extract_doc(&document, cap) {
        Ok(extraction) => extraction,
        Err(message) => {
            report.status = if message.starts_with("panic") {
                "panic"
            } else {
                "error"
            }
            .into();
            report.error_class = Some(classify(&message).into());
            report.error = Some(trim(&message));
            return report;
        }
    };
    fill_extraction(&mut report, &extraction);
    report.identity = Some(align(&extraction, &extraction));
    if bytes.len() > 25_000_000 {
        report.lopdf_error = Some("skipped: structural rewrite is not run above 25MB".into());
    } else {
        match lopdf_rewrite(&bytes) {
            Ok(rewritten) => match extract_bytes(&rewritten, cap) {
                Ok(again) => report.lopdf_rewrite = Some(align(&extraction, &again)),
                Err(message) => report.lopdf_error = Some(trim(&message)),
            },
            Err(message) => report.lopdf_error = Some(trim(&message)),
        }
    }
    let last_page = cap.or_else(|| u32::try_from(pdf_pages).ok());
    report.text_cross_check = Some(poppler_cross_check(path, &extraction, last_page));
    let render_n = cli.render_pages.max(1).min(extraction.pages.len() as u32);
    if render_n > 0 {
        let copy = std::env::temp_dir().join(format!("rpt-qa-{}-identity.pdf", doc.id));
        if fs::write(&copy, &bytes).is_ok() {
            let work = cli.out.join("render").join(&doc.id);
            report.render = Some(compare_renders(path, &copy, &extraction, render_n, &work));
            let _ = fs::remove_file(&copy);
        }
    }
    report.status = "ok".into();
    report
}

fn extract_doc(document: &PdfDocument, max_pages: Option<u32>) -> Result<Extraction, String> {
    let extracted = catch_unwind(AssertUnwindSafe(|| {
        let opts = ExtractOptions {
            max_pages,
            ..ExtractOptions::default()
        };
        document.extract_with(&opts)
    }));
    match extracted {
        Ok(extraction) => Ok(extraction),
        Err(panic) => Err(format!("panic: {}", panic_message(panic))),
    }
}

fn extract_bytes(bytes: &[u8], max_pages: Option<u32>) -> Result<Extraction, String> {
    let opened = catch_unwind(AssertUnwindSafe(|| PdfDocument::open_bytes(bytes)));
    let document = match opened {
        Ok(Ok(document)) => document,
        Ok(Err(err)) => return Err(err.to_string()),
        Err(panic) => return Err(format!("panic: {}", panic_message(panic))),
    };
    extract_doc(&document, max_pages)
}

fn fill_extraction(report: &mut FileReport, extraction: &Extraction) {
    report.extracted_pages = extraction.pages.len();
    report.glyphs = extraction.glyphs.len();
    report.unmapped = extraction
        .glyphs
        .iter()
        .filter(|glyph| glyph.unmapped)
        .count();
    report.unmapped_ratio = if report.glyphs == 0 {
        0.0
    } else {
        report.unmapped as f32 / report.glyphs as f32
    };
    report.unmapped_fonts = unmapped_fonts(extraction);
    report.diagnostics = extraction.diagnostics.len();
    report.diagnostic_samples = extraction
        .diagnostics
        .iter()
        .take(5)
        .map(|item| trim(&item.message))
        .collect();
    let coverage = extraction.coverage_report();
    report.coverage_complete = coverage.complete;
    report.pending = coverage.pending;
    report.observed_tags = observed_tags(extraction);
}

fn lopdf_rewrite(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let saved = catch_unwind(AssertUnwindSafe(|| {
        let mut doc = lopdf::Document::load_mem(bytes).map_err(|err| err.to_string())?;
        let mut out = Vec::new();
        doc.save_to(&mut out).map_err(|err| err.to_string())?;
        Ok(out)
    }));
    match saved {
        Ok(result) => result,
        Err(panic) => Err(format!("panic: {}", panic_message(panic))),
    }
}

fn empty_report(doc: &Document) -> FileReport {
    FileReport {
        id: doc.id.clone(),
        url: doc.url.clone(),
        source: doc.source.clone(),
        title: doc.title.clone(),
        license: doc.license.clone(),
        tags: doc.tags.clone(),
        observed_tags: Vec::new(),
        bytes: doc.bytes,
        pdf_pages: doc.pages.map(|pages| pages as usize),
        extracted_pages: 0,
        truncated: false,
        status: "error".into(),
        error_class: None,
        error: None,
        glyphs: 0,
        unmapped: 0,
        unmapped_ratio: 0.0,
        unmapped_fonts: Vec::new(),
        diagnostics: 0,
        diagnostic_samples: Vec::new(),
        coverage_complete: false,
        pending: 0,
        identity: None,
        lopdf_rewrite: None,
        lopdf_error: None,
        text_cross_check: None,
        render: None,
    }
}

fn summarize(docs: &[Document], reports: &[FileReport]) -> Summary {
    let mut by_source: Vec<Count> = Vec::new();
    for doc in docs {
        bump(&mut by_source, &doc.source);
    }
    let mut by_tag: Vec<Count> = Vec::new();
    for doc in docs {
        for tag in &doc.tags {
            bump(&mut by_tag, tag);
        }
    }
    let mut by_observed: Vec<Count> = Vec::new();
    for report in reports {
        for tag in &report.observed_tags {
            bump(&mut by_observed, tag);
        }
    }
    by_source.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(&b.name)));
    by_tag.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(&b.name)));
    by_observed.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(&b.name)));
    let poppler: Vec<f32> = reports
        .iter()
        .filter_map(|report| {
            report
                .text_cross_check
                .as_ref()
                .and_then(|check| check.ours_in_reference)
        })
        .collect();
    let poppler_in_ours: Vec<f32> = reports
        .iter()
        .filter_map(|report| {
            report
                .text_cross_check
                .as_ref()
                .and_then(|check| check.reference_in_ours)
        })
        .collect();
    let ssim: Vec<f32> = reports
        .iter()
        .filter_map(|report| report.render.as_ref())
        .flat_map(|render| render.pages.iter().filter_map(|page| page.ssim))
        .collect();
    Summary {
        documents: reports.len(),
        extracted: reports
            .iter()
            .filter(|report| report.status == "ok")
            .count(),
        open_failures: reports
            .iter()
            .filter(|report| report.status == "error" || report.status == "missing")
            .count(),
        panics: reports
            .iter()
            .filter(|report| report.status == "panic")
            .count(),
        glyphs: reports.iter().map(|report| report.glyphs).sum(),
        unmapped: reports.iter().map(|report| report.unmapped).sum(),
        identity_text_matches: reports
            .iter()
            .filter(|report| report.identity.as_ref().is_some_and(|item| item.text_match))
            .count(),
        identity_style_clean: reports
            .iter()
            .filter(|report| {
                report.identity.as_ref().is_some_and(|item| {
                    item.style_mismatches == 0 && item.position_max_delta == 0.0
                })
            })
            .count(),
        lopdf_text_matches: reports
            .iter()
            .filter(|report| {
                report
                    .lopdf_rewrite
                    .as_ref()
                    .is_some_and(|item| item.text_match)
            })
            .count(),
        lopdf_attempted: reports
            .iter()
            .filter(|report| report.lopdf_rewrite.is_some() || lopdf_failed(report))
            .count(),
        lopdf_failures: reports.iter().filter(|report| lopdf_failed(report)).count(),
        lopdf_skipped: reports
            .iter()
            .filter(|report| lopdf_skipped(report))
            .count(),
        mean_ours_in_poppler: mean(&poppler),
        mean_poppler_in_ours: mean(&poppler_in_ours),
        mean_identity_ssim: mean(&ssim),
        truncated: reports.iter().filter(|report| report.truncated).count(),
        by_source,
        by_tag,
        by_observed_tag: by_observed,
        failures: reports
            .iter()
            .filter(|report| report.status != "ok")
            .map(|report| Failure {
                id: report.id.clone(),
                class: report
                    .error_class
                    .clone()
                    .unwrap_or_else(|| "unknown".into()),
                message: report.error.clone().unwrap_or_default(),
            })
            .collect(),
    }
}

fn markdown(summary: &Summary, reports: &[FileReport]) -> String {
    let mut out = String::new();
    out.push_str("# Corpus fidelity report\n\n");
    out.push_str("Identity checks copy the original PDF bytes and compare extraction, per-glyph style, and rendered pages. ");
    out.push_str(
        "The lopdf rewrite is a separate structural round-trip. It is not translated output. ",
    );
    out.push_str(
        "PDFium is not installed; text cross-check uses Poppler `pdftotext` when present. ",
    );
    out.push_str(
        "Extraction leaves every glyph `pending`, so `coverage_complete` stays false until a later milestone rewrites or explicitly keeps each glyph. That is the conservation baseline, not a failed extract.\n\n",
    );
    out.push_str(&format!(
        "- Documents: {}\n- Extracted: {}\n- Open failures: {}\n- Panics: {}\n- Glyphs: {}\n- Unmapped glyphs: {}\n- Page-capped documents: {}\n- Identity text matches: {}\n- Identity style/position clean: {}\n- lopdf rewrite text matches: {} of {} attempted\n- lopdf rewrite failures: {}\n- lopdf rewrite skipped above 25MB: {}\n- Mean fraction of our characters found by Poppler: {}\n- Mean fraction of Poppler characters found by us: {}\n- Mean identity-page SSIM: {}\n\n",
        summary.documents,
        summary.extracted,
        summary.open_failures,
        summary.panics,
        summary.glyphs,
        summary.unmapped,
        summary.truncated,
        summary.identity_text_matches,
        summary.identity_style_clean,
        summary.lopdf_text_matches,
        summary.lopdf_attempted,
        summary.lopdf_failures,
        summary.lopdf_skipped,
        fmt_opt(summary.mean_ours_in_poppler),
        fmt_opt(summary.mean_poppler_in_ours),
        fmt_opt(summary.mean_identity_ssim),
    ));
    out.push_str("## By source\n\n");
    for item in &summary.by_source {
        out.push_str(&format!("- {}: {}\n", item.name, item.count));
    }
    out.push_str("\n## Manifest tags\n\n");
    for item in &summary.by_tag {
        out.push_str(&format!("- {}: {}\n", item.name, item.count));
    }
    out.push_str("\n## Observed feature tags\n\n");
    out.push_str(
        "These come from the extracted glyphs (column gutters, font names, Unicode ranges), not from the manifest.\n\n",
    );
    for item in &summary.by_observed_tag {
        out.push_str(&format!("- {}: {}\n", item.name, item.count));
    }
    if !summary.failures.is_empty() {
        out.push_str("\n## Failures\n\n");
        for failure in &summary.failures {
            out.push_str(&format!(
                "- `{}` ({}): {}\n",
                failure.id, failure.class, failure.message
            ));
        }
    }
    out.push_str("\n## Per file\n\n");
    out.push_str(
        "| id | status | pages | glyphs | unmapped | poppler | identity SSIM | lopdf text |\n",
    );
    out.push_str("| --- | --- | --- | --- | --- | --- | --- | --- |\n");
    for report in reports {
        let poppler = report
            .text_cross_check
            .as_ref()
            .and_then(|check| check.ours_in_reference)
            .map(|value| format!("{value:.3}"))
            .unwrap_or_else(|| "-".into());
        let ssim = report
            .render
            .as_ref()
            .and_then(|render| render.pages.first())
            .and_then(|page| page.ssim)
            .map(|value| format!("{value:.4}"))
            .unwrap_or_else(|| "-".into());
        let lopdf = if let Some(align) = &report.lopdf_rewrite {
            if align.text_match {
                "match"
            } else {
                "differ"
            }
        } else if lopdf_skipped(report) {
            "skip"
        } else if report.lopdf_error.is_some() {
            "error"
        } else {
            "-"
        };
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {} | {} | {} |\n",
            report.id,
            report.status,
            report
                .pdf_pages
                .map(|n| n.to_string())
                .unwrap_or_else(|| "-".into()),
            report.glyphs,
            report.unmapped,
            poppler,
            ssim,
            lopdf,
        ));
    }
    let high_unmapped: Vec<&FileReport> = reports
        .iter()
        .filter(|report| report.unmapped_ratio >= 0.02 && report.unmapped > 0)
        .collect();
    if !high_unmapped.is_empty() {
        out.push_str("\n## Unmapped glyphs\n\n");
        out.push_str(
            "A glyph is kept and flagged when no Unicode mapping is found. The large clusters are older pdfTeX files: Computer Modern Type 1 subsets with Builtin or Custom encodings and no ToUnicode CMap, so OT1/OML/OMS codes stay unmapped. The Word-produced NIST file is the other cluster (subsetted Times and Arial). Poppler still recovers most of those characters.\n\n",
        );
        for report in high_unmapped {
            let fonts = report
                .unmapped_fonts
                .iter()
                .take(4)
                .map(|item| format!("{} ({})", item.name, item.count))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "- `{}`: {} / {} ({:.1}%) — {}\n",
                report.id,
                report.unmapped,
                report.glyphs,
                report.unmapped_ratio * 100.0,
                fonts
            ));
        }
    }
    let diagnosed: Vec<&FileReport> = reports
        .iter()
        .filter(|report| report.diagnostics > 0)
        .collect();
    if !diagnosed.is_empty() {
        out.push_str("\n## Diagnostics\n\n");
        for report in diagnosed {
            let sample = report.diagnostic_samples.join("; ");
            out.push_str(&format!(
                "- `{}`: {} — {}\n",
                report.id, report.diagnostics, sample
            ));
        }
    }
    out.push_str("\nPer-page scores and the full font lists are in the JSON report.\n");
    out
}

fn unmapped_fonts(extraction: &Extraction) -> Vec<Count> {
    let mut counts = Vec::new();
    for glyph in extraction.glyphs.iter().filter(|glyph| glyph.unmapped) {
        let name = if glyph.font_name.is_empty() {
            glyph.font_resource.as_str()
        } else {
            glyph.font_name.as_str()
        };
        bump(&mut counts, name);
    }
    counts.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(&b.name)));
    counts.truncate(8);
    counts
}

fn lopdf_skipped(report: &FileReport) -> bool {
    report
        .lopdf_error
        .as_ref()
        .is_some_and(|message| message.starts_with("skipped:"))
}

fn lopdf_failed(report: &FileReport) -> bool {
    if lopdf_skipped(report) {
        return false;
    }
    report.lopdf_error.is_some()
        || report
            .lopdf_rewrite
            .as_ref()
            .is_some_and(|item| !item.text_match)
}

fn bump(counts: &mut Vec<Count>, name: &str) {
    if let Some(item) = counts.iter_mut().find(|item| item.name == name) {
        item.count += 1;
    } else {
        counts.push(Count {
            name: name.to_string(),
            count: 1,
        });
    }
}

fn mean(values: &[f32]) -> Option<f32> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f32>() / values.len() as f32)
    }
}

fn fmt_opt(value: Option<f32>) -> String {
    value
        .map(|item| format!("{item:.4}"))
        .unwrap_or_else(|| "n/a".into())
}

fn classify(message: &str) -> &'static str {
    let lower = message.to_lowercase();
    if lower.contains("panic") {
        "panic"
    } else if lower.contains("encrypt") || lower.contains("password") {
        "encrypted"
    } else if lower.contains("xref") || lower.contains("trailer") || lower.contains("startxref") {
        "xref-or-trailer"
    } else if lower.contains("flate") || lower.contains("decompress") || lower.contains("filter") {
        "stream-filter"
    } else if lower.contains("io error") || lower.contains("os error") {
        "io"
    } else {
        "parse"
    }
}

fn trim(message: &str) -> String {
    let mut text = message.replace('\n', " ");
    if text.len() > 300 {
        text.truncate(300);
    }
    text
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn panic_message(err: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = err.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = err.downcast_ref::<String>() {
        message.clone()
    } else {
        "panic".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_empty_and_hello_identity() {
        assert_eq!(
            sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/hello.pdf");
        let doc = Document {
            id: "hello".into(),
            url: String::new(),
            source: "fixture".into(),
            title: "Hello".into(),
            license: "test".into(),
            sha256: String::new(),
            bytes: 0,
            pages: Some(1),
            tags: vec!["fixture".into()],
            ci: true,
        };
        let cli = Cli {
            manifest: PathBuf::new(),
            cache: PathBuf::new(),
            out: std::env::temp_dir().join("rpt-qa-hello"),
            ci: true,
            render_pages: 1,
            full_extract_until: 40,
            page_cap: 12,
        };
        let report = analyze_document(&doc, &path, &cli);
        assert_eq!(report.status, "ok", "{:?}", report.error);
        assert_eq!(report.glyphs, 5);
        assert_eq!(report.unmapped, 0);
        let identity = report.identity.unwrap();
        assert!(identity.text_match);
        assert_eq!(identity.style_mismatches, 0);
        assert_eq!(identity.position_max_delta, 0.0);
        let render = report.render.unwrap();
        assert!(render.available, "{}", render.note);
        assert!(
            render.pages[0].exact_ratio > 0.999,
            "{}",
            render.pages[0].exact_ratio
        );
        assert!(render.pages[0].ssim.unwrap() > 0.999);
    }
}
