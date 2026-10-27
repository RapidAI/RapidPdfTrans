//! `rpt extract file.pdf` prints per-glyph JSON and the coverage report.
//! `rpt translate file.pdf --output out.pdf` translates and writes a PDF.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use clap::{Parser, Subcommand};
use rpt_core::{
    failing_layout_message, parse_font_spec, parse_glossary_csv, replay_extraction,
    translate_extraction, CjkStyle, ExtractOptions, OpenOptions, OutputMode, PdfDocument,
    ProgressMode, RewriteOptions, TranslateOptions, TranslatedSegment, TranslatorBackend,
};

#[derive(Parser)]
#[command(name = "rpt", version, about = "Format-preserving PDF translation")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Command {
    /// Dump per-glyph JSON and the coverage report.
    Extract {
        path: PathBuf,
        /// Accepted for compatibility. JSON is always written to stdout.
        #[arg(long)]
        json: bool,
        #[arg(long)]
        compact: bool,
        /// Open/extract options as a JSON object. `api_key` is ignored.
        #[arg(long, default_value = "")]
        options: String,
    },
    /// Translate extracted text. `--output` writes the translated PDF.
    Translate {
        path: PathBuf,
        /// Write the translated PDF here. The mode is `--mode` (default: replace).
        #[arg(long)]
        output: Option<PathBuf>,
        /// Also write a bilingual PDF from the same translation. Layout is `--layout`.
        #[arg(long)]
        bilingual_output: Option<PathBuf>,
        /// `replace` (default), `bilingual`, `side-by-side`, `alternating`, or `overlay`.
        #[arg(long, default_value = "replace")]
        mode: String,
        /// Used with `--mode bilingual` or `--bilingual`: `side-by-side`, `alternating`, or `overlay`.
        #[arg(long, default_value = "side-by-side")]
        layout: String,
        /// Bilingual output. Layout comes from `--layout` (default: side-by-side).
        #[arg(long)]
        bilingual: bool,
        /// Extract and translate only the first N pages.
        #[arg(long)]
        max_pages: Option<u32>,
        #[arg(long = "from", default_value = "en")]
        source_lang: String,
        #[arg(long = "to", default_value = "zh")]
        target_lang: String,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        base_url: Option<String>,
        /// Glossary entry `source=target`, or a CSV file (`src,tgt`). Repeat for more than one.
        #[arg(long = "glossary", value_name = "SOURCE=TARGET|FILE.csv")]
        glossary: Vec<String>,
        /// Extra system prompt. A path to an existing file is read; otherwise the text is used as written.
        #[arg(long)]
        system_prompt: Option<String>,
        /// Per-request timeout in seconds. A stalled call is retried with backoff. Default 60.
        #[arg(long, default_value_t = 60)]
        timeout_secs: u64,
        /// Stderr progress: `text` (default), `json`, or `off`.
        #[arg(long, default_value = "text")]
        progress: String,
        /// Translate the References section. By default that section is kept unchanged.
        #[arg(long)]
        translate_references: bool,
        /// Translate text inside figures. By default only the figure caption is translated.
        #[arg(long)]
        translate_figures: bool,
        /// Translate table cells as well as headers. By default the caption and the column header are translated, and cells stay in the source language.
        #[arg(long)]
        translate_tables: bool,
        /// CJK family: `serif` (default, Noto Serif CJK SC) or `sans` (Noto Sans CJK SC).
        /// `RPT_CJK_STYLE` is used when this flag is omitted.
        #[arg(long, value_parser = parse_cjk_style_arg)]
        cjk_style: Option<String>,
        /// Regular CJK font (TTF, TTC, or OTF). Wins over `--cjk-style`.
        /// Append `#N` to select face N of a collection (`msyh.ttc#0`).
        #[arg(long)]
        cjk_font: Option<PathBuf>,
        /// Sans CJK font for regular sans source text. Wins over `--cjk-font` for that text.
        #[arg(long)]
        cjk_sans: Option<PathBuf>,
        /// Bold CJK font for titles and bold source faces. Wins over `--cjk-style`.
        /// `#N` selects a collection face.
        #[arg(long)]
        cjk_bold_font: Option<PathBuf>,
        /// Chinese body size as a fraction of the English size. Default 0.90.
        #[arg(long)]
        cjk_size_scale: Option<f32>,
        /// Chinese baseline distance in ems of that size. Default 1.60.
        #[arg(long)]
        cjk_leading: Option<f32>,
        /// Segments per model call.
        #[arg(long, default_value_t = 8)]
        batch_size: usize,
        /// Parallel translation requests. Default 4, or `RPT_TRANSLATE_JOBS`.
        /// The count is capped at 8. Layout and rewrite stay single-threaded.
        #[arg(long)]
        jobs: Option<usize>,
        /// Reuse translations from a previous `rpt translate` JSON file.
        /// Segments are matched by exact source text. No model is called.
        #[arg(long)]
        replay: Option<PathBuf>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        compact: bool,
        /// Write the full per-glyph JSON to this file. Stdout stays a summary.
        #[arg(long)]
        detail: Option<PathBuf>,
        /// Print the full per-glyph JSON on stdout instead of the summary.
        #[arg(long)]
        glyphs: bool,
        /// Exit 1 when the PDF was written but the layout check found an
        /// overlap, a wide gap, or another layout failure. By default those
        /// are warnings: they go to stderr and to `layout_warnings` in the
        /// JSON, and the command exits 0.
        #[arg(long)]
        strict: bool,
    },
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Extract {
            path,
            json: _,
            compact,
            options,
        } => run_extract(&path, compact, &options),
        Command::Translate {
            path,
            output,
            bilingual_output,
            mode,
            layout,
            bilingual,
            max_pages,
            source_lang,
            target_lang,
            model,
            base_url,
            glossary,
            system_prompt,
            timeout_secs,
            progress,
            translate_references,
            translate_figures,
            translate_tables,
            cjk_style,
            cjk_font,
            cjk_sans,
            cjk_bold_font,
            cjk_size_scale,
            cjk_leading,
            batch_size,
            jobs,
            replay,
            json: _,
            compact,
            detail,
            glyphs,
            strict,
        } => {
            let glossary = match parse_glossary(&glossary) {
                Ok(pairs) => pairs,
                Err(err) => return fail(err),
            };
            let system_prompt = match system_prompt.as_deref().map(read_system_prompt).transpose() {
                Ok(text) => text,
                Err(err) => return fail(err),
            };
            let progress = match ProgressMode::parse(&progress) {
                Ok(mode) => mode,
                Err(err) => return fail(err),
            };
            if timeout_secs == 0 {
                return fail("timeout-secs must be at least 1");
            }
            if matches!(jobs, Some(0)) {
                return fail("jobs must be at least 1");
            }
            let output_mode = if bilingual && mode == "replace" {
                OutputMode::parse("bilingual", Some(&layout))
            } else {
                OutputMode::parse(&mode, Some(&layout))
            };
            let output_mode = match output_mode {
                Ok(mode) => mode,
                Err(err) => return fail(err.to_string()),
            };
            let bilingual_mode = OutputMode::parse("bilingual", Some(&layout)).ok();
            if let Some(path) = cjk_font.as_ref() {
                let spec = parse_font_spec(&path.display().to_string());
                if !spec.path.is_file() {
                    return fail(format!("CJK font not found: {}", spec.path.display()));
                }
            }
            if let Some(path) = cjk_sans.as_ref() {
                let spec = parse_font_spec(&path.display().to_string());
                if !spec.path.is_file() {
                    return fail(format!("CJK sans font not found: {}", spec.path.display()));
                }
            }
            if let Some(path) = cjk_bold_font.as_ref() {
                let spec = parse_font_spec(&path.display().to_string());
                if !spec.path.is_file() {
                    return fail(format!("CJK bold font not found: {}", spec.path.display()));
                }
            }
            if let Err(err) = CjkStyle::resolve(cjk_style.as_deref()) {
                return fail(err);
            }
            run_translate(
                &path,
                replay.as_deref(),
                TranslateOptions {
                    source_lang,
                    target_lang,
                    model,
                    base_url,
                    glossary,
                    skip_references: !translate_references,
                    skip_figures: !translate_figures,
                    skip_tables: !translate_tables,
                    cjk_style,
                    cjk_font: cjk_font.as_ref().map(|path| path.display().to_string()),
                    cjk_sans: cjk_sans.as_ref().map(|path| path.display().to_string()),
                    cjk_bold_font: cjk_bold_font
                        .as_ref()
                        .map(|path| path.display().to_string()),
                    cjk_size_scale: cjk_size_scale.unwrap_or(0.0),
                    cjk_leading: cjk_leading.unwrap_or(0.0),
                    batch_size,
                    jobs: jobs.unwrap_or(0),
                    output_mode,
                    timeout_secs,
                    system_prompt,
                    progress,
                    ..TranslateOptions::default()
                },
                compact,
                output,
                bilingual_output,
                bilingual_mode,
                max_pages,
                detail,
                glyphs,
                strict,
            )
        }
    }
}

fn run_extract(path: &PathBuf, compact: bool, options: &str) -> ExitCode {
    let open_opts = match OpenOptions::from_json(options) {
        Ok(opts) => opts,
        Err(err) => return fail(err),
    };
    let extract_opts = match ExtractOptions::from_json(options) {
        Ok(opts) => opts,
        Err(err) => return fail(err),
    };
    let doc = match PdfDocument::open_with(path, &open_opts) {
        Ok(doc) => doc,
        Err(err) => return fail(err),
    };
    let extraction = doc.extract_with(&extract_opts);
    let value = match extraction.to_json_value() {
        Ok(value) => value,
        Err(err) => return fail(err),
    };
    emit(&value, compact)
}

#[allow(clippy::too_many_arguments)]
fn run_translate(
    path: &PathBuf,
    replay: Option<&std::path::Path>,
    opts: TranslateOptions,
    compact: bool,
    output: Option<PathBuf>,
    bilingual_output: Option<PathBuf>,
    bilingual_mode: Option<OutputMode>,
    max_pages: Option<u32>,
    detail: Option<PathBuf>,
    glyphs: bool,
    strict: bool,
) -> ExitCode {
    let mut doc = match PdfDocument::open(path) {
        Ok(doc) => doc,
        Err(err) => return fail(err),
    };
    let mut extraction = doc.extract_with(&ExtractOptions {
        max_pages,
        ..ExtractOptions::default()
    });
    let (report, backend, model, endpoint) = if let Some(replay) = replay {
        let saved = match load_replay(replay) {
            Ok(saved) => saved,
            Err(err) => return fail(err),
        };
        let report = match replay_extraction(&mut extraction, &opts, &saved) {
            Ok(report) => report,
            Err(err) => return fail(err),
        };
        (
            report,
            "replay".to_string(),
            "replay".to_string(),
            String::new(),
        )
    } else {
        let client = match TranslatorBackend::from_env(&opts) {
            Ok(client) => client,
            Err(err) => return fail(err),
        };
        let report = match translate_extraction(&mut extraction, &opts, &client) {
            Ok(report) => report,
            Err(err) => return fail(err),
        };
        (
            report,
            client.label().to_string(),
            client.model().to_string(),
            client.endpoint().to_string(),
        )
    };
    let mut layout_failure: Option<String> = None;
    let mut warnings: Vec<String> = Vec::new();
    if let Some(output) = output.as_ref() {
        let _pulse = PhasePulse::start(opts.progress, "replace");
        if let Err(err) = doc.rewrite(
            &mut extraction,
            &report,
            &RewriteOptions::from_translate(&opts, opts.output_mode),
        ) {
            return fail(err);
        }
        drop(_pulse);
        if let Err(err) = doc.save_file(output) {
            return fail(err);
        }
        note_wrote(opts.progress, output);
        if let Some(warning) = extraction.note_incomplete_coverage() {
            eprintln!("rpt: warning: {warning}");
            warnings.push(warning);
        }
        warn_cjk_font(&extraction);
        layout_failure = failing_layout_message(&extraction.diagnostics);
    }
    // The translated document is no longer needed. Drop it before the
    // bilingual pass opens the source again.
    drop(doc);
    if let Some(bilingual_output) = bilingual_output.as_ref() {
        let Some(mode) = bilingual_mode else {
            return fail("bilingual layout is invalid");
        };
        let mut copy = match PdfDocument::open(path) {
            Ok(doc) => doc,
            Err(err) => return fail(err),
        };
        let mut bilingual_extraction = copy.extract_with(&ExtractOptions {
            max_pages,
            ..ExtractOptions::default()
        });
        let _pulse = PhasePulse::start(opts.progress, "bilingual");
        if let Err(err) = copy.rewrite(
            &mut bilingual_extraction,
            &report,
            &RewriteOptions::from_translate(&opts, mode),
        ) {
            return fail(err);
        }
        drop(_pulse);
        if let Err(err) = copy.save_file(bilingual_output) {
            return fail(err);
        }
        note_wrote(opts.progress, bilingual_output);
        if let Some(warning) = bilingual_extraction.note_incomplete_coverage() {
            eprintln!("rpt: warning: {warning}");
        }
        warn_cjk_font(&bilingual_extraction);
        if let Some(message) = failing_layout_message(&bilingual_extraction.diagnostics) {
            layout_failure = Some(match layout_failure {
                Some(have) => format!("{have}; {message}"),
                None => message,
            });
        }
    }
    let translator = serde_json::json!({
        "model": model,
        "backend": backend,
        "base_url": endpoint,
        "calls": report.calls,
        "cache_hits": report.cache_hits,
    });
    if let Some(message) = layout_failure.as_ref() {
        warnings.push(message.clone());
        if !strict {
            eprintln!("rpt: warning: layout check: {message}");
        }
    }
    let mut summary = match extraction.to_summary_value() {
        Ok(value) => value,
        Err(err) => return fail(err),
    };
    if let Some(obj) = summary.as_object_mut() {
        obj.insert(
            "translations".into(),
            serde_json::to_value(&report.segments).unwrap_or(serde_json::Value::Null),
        );
        obj.insert("translator".into(), translator.clone());
        obj.insert("layout_warnings".into(), serde_json::json!(warnings));
    }
    if detail.is_some() || glyphs {
        let mut full = match extraction.to_json_value() {
            Ok(value) => value,
            Err(err) => return fail(err),
        };
        if let Some(obj) = full.as_object_mut() {
            obj.insert(
                "translations".into(),
                serde_json::to_value(&report.segments).unwrap_or(serde_json::Value::Null),
            );
            obj.insert("translator".into(), translator);
            obj.insert("layout_warnings".into(), serde_json::json!(warnings));
        }
        if let Some(path) = detail.as_ref() {
            let text = if compact {
                serde_json::to_string(&full)
            } else {
                serde_json::to_string_pretty(&full)
            };
            let text = match text {
                Ok(text) => text,
                Err(err) => return fail(err),
            };
            if let Err(err) = std::fs::write(path, text) {
                return fail(format!("failed to write {}: {err}", path.display()));
            }
        }
        if glyphs {
            if let Some(message) = strict_failure(strict, layout_failure) {
                return fail(message);
            }
            return emit(&full, compact);
        }
    }
    if let Some(message) = strict_failure(strict, layout_failure) {
        // The JSON is still printed so a strict caller sees the same report.
        let _ = emit(&summary, compact);
        return fail(message);
    }
    emit(&summary, compact)
}

/// A layout failure fails the command only under `--strict`. The PDF was
/// already written; without `--strict` it is a warning.
fn strict_failure(strict: bool, layout_failure: Option<String>) -> Option<String> {
    layout_failure.filter(|_| strict)
}

/// A rewrite of a large PDF can sit quiet for a minute. Emit a line every
/// five seconds so that wait is not mistaken for a stalled request.
struct PhasePulse {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl PhasePulse {
    fn start(progress: ProgressMode, label: &str) -> Self {
        if progress == ProgressMode::Off {
            return Self {
                stop: Arc::new(AtomicBool::new(true)),
                handle: None,
            };
        }
        let json = progress == ProgressMode::Json;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let label = label.to_string();
        let started = Instant::now();
        if json {
            eprintln!(
                "{}",
                serde_json::json!({"event": "rewriting", "phase": label, "elapsed_secs": 0})
            );
        } else {
            eprintln!("rpt: rewriting ({label})");
        }
        let handle = std::thread::spawn(move || {
            let mut ticks = 0u32;
            while !thread_stop.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(200));
                if thread_stop.load(Ordering::Relaxed) {
                    break;
                }
                ticks += 1;
                if ticks < 25 {
                    continue;
                }
                ticks = 0;
                let elapsed = started.elapsed().as_secs();
                if json {
                    eprintln!(
                        "{}",
                        serde_json::json!({"event": "rewriting", "phase": label, "elapsed_secs": elapsed})
                    );
                } else {
                    eprintln!("rpt: rewriting, {elapsed}s elapsed ({label})");
                }
            }
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for PhasePulse {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn note_wrote(progress: ProgressMode, path: &std::path::Path) {
    if progress == ProgressMode::Off {
        return;
    }
    if progress == ProgressMode::Json {
        eprintln!(
            "{}",
            serde_json::json!({"event": "wrote", "path": path.display().to_string()})
        );
    } else {
        eprintln!("rpt: wrote {}", path.display());
    }
}

fn warn_cjk_font(extraction: &rpt_core::Extraction) {
    for item in &extraction.diagnostics {
        if item.message.starts_with("CJK font") {
            eprintln!("rpt: warning: {}", item.message);
        }
    }
}

fn load_replay(path: &std::path::Path) -> Result<Vec<TranslatedSegment>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("failed to read replay file {}: {err}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|err| format!("replay file {} is not JSON: {err}", path.display()))?;
    let items = value
        .as_array()
        .or_else(|| value.get("translations").and_then(|item| item.as_array()))
        .ok_or_else(|| "replay file needs a translations array".to_string())?;
    items
        .iter()
        .map(|item| {
            serde_json::from_value(item.clone())
                .map_err(|err| format!("replay segment is invalid: {err}"))
        })
        .collect()
}

fn parse_glossary(entries: &[String]) -> Result<Vec<(String, String)>, String> {
    let mut pairs = Vec::new();
    for entry in entries {
        if std::path::Path::new(entry).is_file() {
            let text = std::fs::read_to_string(entry)
                .map_err(|err| format!("failed to read glossary {}: {err}", entry))?;
            pairs.extend(parse_glossary_csv(&text).map_err(|err| err.to_string())?);
            continue;
        }
        let Some((source, target)) = entry.split_once('=') else {
            return Err(format!(
                "glossary entry `{entry}` is not source=target or a CSV file"
            ));
        };
        if source.is_empty() {
            return Err(format!("glossary entry `{entry}` has an empty source"));
        }
        pairs.push((source.to_string(), target.to_string()));
    }
    Ok(pairs)
}

fn read_system_prompt(value: &str) -> Result<String, String> {
    let path = value.strip_prefix('@').unwrap_or(value);
    let as_path = std::path::Path::new(path);
    if as_path.is_file() {
        std::fs::read_to_string(as_path)
            .map_err(|err| format!("failed to read system prompt {}: {err}", as_path.display()))
    } else if value.starts_with('@') {
        Err(format!("system prompt file not found: {path}"))
    } else {
        Ok(value.to_string())
    }
}

fn emit(value: &serde_json::Value, compact: bool) -> ExitCode {
    let text = if compact {
        serde_json::to_string(value)
    } else {
        serde_json::to_string_pretty(value)
    };
    match text {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(err) => fail(err),
    }
}

fn parse_cjk_style_arg(raw: &str) -> Result<String, String> {
    CjkStyle::parse(raw).map(|style| style.as_str().to_string())
}

fn fail(err: impl std::fmt::Display) -> ExitCode {
    let mut message = err.to_string();
    for name in [
        "RPT_LLM_API_KEY",
        "RPT_GOOGLE_API_KEY",
        "RPT_GOOGLE_ACCESS_TOKEN",
    ] {
        if let Ok(key) = std::env::var(name) {
            if !key.is_empty() {
                message = message.replace(&key, "[redacted]");
            }
        }
    }
    eprintln!("rpt: {message}");
    ExitCode::from(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn a_layout_failure_is_a_warning_unless_strict() {
        let message = Some("glyph overlap 11.0pt".to_string());
        assert_eq!(strict_failure(false, message.clone()), None);
        assert_eq!(strict_failure(true, message.clone()), message);
        assert_eq!(strict_failure(true, None), None);
    }

    #[test]
    fn strict_is_opt_in() {
        let parse = |extra: &[&str]| {
            let mut args = vec!["rpt", "translate", "in.pdf"];
            args.extend_from_slice(extra);
            match Cli::parse_from(args).command {
                Command::Translate { strict, .. } => strict,
                _ => unreachable!(),
            }
        };
        assert!(!parse(&[]));
        assert!(parse(&["--strict"]));
    }
}
