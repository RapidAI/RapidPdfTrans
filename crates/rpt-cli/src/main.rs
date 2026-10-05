//! `rpt extract file.pdf` prints per-glyph JSON and the coverage report.
//! `rpt translate file.pdf --output out.pdf` translates and writes a PDF.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use rpt_core::{
    translate_extraction, ExtractOptions, OpenOptions, OutputMode, PdfDocument, RewriteOptions,
    TranslateOptions, TranslatorBackend,
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
        /// Glossary entry `source=target`. Repeat the flag for more than one.
        #[arg(long = "glossary", value_name = "SOURCE=TARGET")]
        glossary: Vec<String>,
        /// Translate the References section. By default that section is kept unchanged.
        #[arg(long)]
        translate_references: bool,
        /// Translate text inside figures. By default only the figure caption is translated.
        #[arg(long)]
        translate_figures: bool,
        /// Translate table cells and headers. By default only the table caption is translated.
        #[arg(long)]
        translate_tables: bool,
        /// Song/serif CJK font (TTF, TTC, or OTF). Overrides Noto Serif CJK for body text.
        #[arg(long)]
        cjk_font: Option<PathBuf>,
        /// Sans CJK font for regular sans text. Bold text still uses Noto Sans CJK Bold.
        #[arg(long)]
        cjk_sans: Option<PathBuf>,
        /// Chinese body size as a fraction of the English size. Default 0.90.
        #[arg(long)]
        cjk_size_scale: Option<f32>,
        /// Chinese baseline distance in ems of that size. Default 1.60.
        #[arg(long)]
        cjk_leading: Option<f32>,
        /// Segments per model call.
        #[arg(long, default_value_t = 8)]
        batch_size: usize,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        compact: bool,
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
            translate_references,
            translate_figures,
            translate_tables,
            cjk_font,
            cjk_sans,
            cjk_size_scale,
            cjk_leading,
            batch_size,
            json: _,
            compact,
        } => {
            let glossary = match parse_glossary(&glossary) {
                Ok(pairs) => pairs,
                Err(err) => return fail(err),
            };
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
                if !path.is_file() {
                    return fail(format!("CJK font not found: {}", path.display()));
                }
            }
            if let Some(path) = cjk_sans.as_ref() {
                if !path.is_file() {
                    return fail(format!("CJK sans font not found: {}", path.display()));
                }
            }
            run_translate(
                &path,
                TranslateOptions {
                    source_lang,
                    target_lang,
                    model,
                    base_url,
                    glossary,
                    skip_references: !translate_references,
                    skip_figures: !translate_figures,
                    skip_tables: !translate_tables,
                    cjk_font: cjk_font.as_ref().map(|path| path.display().to_string()),
                    cjk_sans: cjk_sans.as_ref().map(|path| path.display().to_string()),
                    cjk_size_scale: cjk_size_scale.unwrap_or(0.0),
                    cjk_leading: cjk_leading.unwrap_or(0.0),
                    batch_size,
                    output_mode,
                    ..TranslateOptions::default()
                },
                compact,
                output,
                bilingual_output,
                bilingual_mode,
                max_pages,
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

fn run_translate(
    path: &PathBuf,
    opts: TranslateOptions,
    compact: bool,
    output: Option<PathBuf>,
    bilingual_output: Option<PathBuf>,
    bilingual_mode: Option<OutputMode>,
    max_pages: Option<u32>,
) -> ExitCode {
    let mut doc = match PdfDocument::open(path) {
        Ok(doc) => doc,
        Err(err) => return fail(err),
    };
    let client = match TranslatorBackend::from_env(&opts) {
        Ok(client) => client,
        Err(err) => return fail(err),
    };
    let mut extraction = doc.extract_with(&ExtractOptions {
        max_pages,
        ..ExtractOptions::default()
    });
    let report = match translate_extraction(&mut extraction, &opts, &client) {
        Ok(report) => report,
        Err(err) => return fail(err),
    };
    if let Some(output) = output.as_ref() {
        if let Err(err) = doc.rewrite(
            &mut extraction,
            &report,
            &RewriteOptions {
                mode: opts.output_mode,
                cjk_serif: opts.cjk_font.as_ref().map(std::path::PathBuf::from),
                cjk_sans: opts.cjk_sans.as_ref().map(std::path::PathBuf::from),
                cjk_size_scale: opts.cjk_size_scale,
                cjk_leading: opts.cjk_leading,
                ..RewriteOptions::default()
            },
        ) {
            return fail(err);
        }
        if let Err(err) = extraction.assert_complete() {
            return fail(err);
        }
        if let Err(err) = doc.save_file(output) {
            return fail(err);
        }
    }
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
        if let Err(err) = copy.rewrite(
            &mut bilingual_extraction,
            &report,
            &RewriteOptions {
                mode,
                cjk_serif: opts.cjk_font.as_ref().map(std::path::PathBuf::from),
                cjk_sans: opts.cjk_sans.as_ref().map(std::path::PathBuf::from),
                cjk_size_scale: opts.cjk_size_scale,
                cjk_leading: opts.cjk_leading,
                ..RewriteOptions::default()
            },
        ) {
            return fail(err);
        }
        if let Err(err) = bilingual_extraction.assert_complete() {
            return fail(err);
        }
        if let Err(err) = copy.save_file(bilingual_output) {
            return fail(err);
        }
    }
    let mut value = match extraction.to_json_value() {
        Ok(value) => value,
        Err(err) => return fail(err),
    };
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "translations".into(),
            serde_json::to_value(&report.segments).unwrap_or(serde_json::Value::Null),
        );
        obj.insert(
            "translator".into(),
            serde_json::json!({
                "model": client.model(),
                "backend": client.label(),
                "base_url": client.endpoint(),
                "calls": report.calls,
                "cache_hits": report.cache_hits,
            }),
        );
    }
    emit(&value, compact)
}

fn parse_glossary(entries: &[String]) -> Result<Vec<(String, String)>, String> {
    let mut pairs = Vec::new();
    for entry in entries {
        let Some((source, target)) = entry.split_once('=') else {
            return Err(format!("glossary entry `{entry}` is not source=target"));
        };
        if source.is_empty() {
            return Err(format!("glossary entry `{entry}` has an empty source"));
        }
        pairs.push((source.to_string(), target.to_string()));
    }
    Ok(pairs)
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
