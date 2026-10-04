//! `rpt extract file.pdf` prints per-glyph JSON and the coverage report.
//! `rpt translate file.pdf` translates text and does not rewrite the PDF.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use rpt_core::{
    translate_extraction, ExtractOptions, LlmTranslator, OpenOptions, PdfDocument, TranslateOptions,
};

#[derive(Parser)]
#[command(name = "rpt", version, about = "Format-preserving PDF translation")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
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
    /// Translate extracted text. The PDF is not rewritten.
    Translate {
        path: PathBuf,
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
            source_lang,
            target_lang,
            model,
            base_url,
            glossary,
            json: _,
            compact,
        } => run_translate(
            &path,
            source_lang,
            target_lang,
            model,
            base_url,
            glossary,
            compact,
        ),
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
    source_lang: String,
    target_lang: String,
    model: Option<String>,
    base_url: Option<String>,
    glossary: Vec<String>,
    compact: bool,
) -> ExitCode {
    let glossary = match parse_glossary(&glossary) {
        Ok(pairs) => pairs,
        Err(err) => return fail(err),
    };
    let opts = TranslateOptions {
        source_lang,
        target_lang,
        model,
        base_url,
        glossary,
        ..TranslateOptions::default()
    };
    let doc = match PdfDocument::open(path) {
        Ok(doc) => doc,
        Err(err) => return fail(err),
    };
    let client = match LlmTranslator::from_env(&opts) {
        Ok(client) => client,
        Err(err) => return fail(err),
    };
    let mut extraction = doc.extract();
    let report = match translate_extraction(&mut extraction, &opts, &client) {
        Ok(report) => report,
        Err(err) => return fail(err),
    };
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
                "base_url": client.base_url(),
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
    if let Ok(key) = std::env::var("RPT_LLM_API_KEY") {
        if !key.is_empty() {
            message = message.replace(&key, "[redacted]");
        }
    }
    eprintln!("rpt: {message}");
    ExitCode::from(1)
}
