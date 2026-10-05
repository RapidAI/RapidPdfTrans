//! Score translated PDFs, or an identity copy of each corpus file.
//!
//! `rpt-bench pair` compares one output to its source.
//! `rpt-bench corpus` scores each cached file against itself. That is the
//! extraction and render ceiling, not a translated-PDF result.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use rpt_qa::score::{score_pair, ScoreOptions};
use serde::Deserialize;

#[derive(Parser)]
#[command(name = "rpt-bench", about = "Objective PDF translation benchmark")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Score one translated PDF against the source PDF.
    Pair {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value_t = 1)]
        render_pages: u32,
        #[arg(long)]
        max_pages: Option<u32>,
    },
    /// Score each manifest PDF against itself.
    Corpus {
        #[arg(long, default_value = "corpus/ci/manifest.json")]
        manifest: PathBuf,
        #[arg(long, default_value = "corpus/ci")]
        cache: PathBuf,
        #[arg(long, default_value = "corpus/benchmarks/ceiling.json")]
        out: PathBuf,
        #[arg(long, default_value_t = 1)]
        render_pages: u32,
        #[arg(long)]
        max_pages: Option<u32>,
    },
}

#[derive(Deserialize)]
struct Manifest {
    documents: Vec<Document>,
}

#[derive(Deserialize)]
struct Document {
    id: String,
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Pair {
            source,
            output,
            render_pages,
            max_pages,
        } => {
            let opts = ScoreOptions {
                render_pages,
                max_pages,
            };
            match score_pair(&source, &output, &opts) {
                Ok(score) => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&score).unwrap_or_else(|_| "{}".into())
                    );
                    if !score.paint_font_ok
                        || score.horizontal_containment < 0.9
                        || score.cjk_extract_ratio < 0.9
                    {
                        eprintln!(
                            "rpt-bench: paint check failed (font_ok={} containment={:.3} cjk_extract={:.3})",
                            score.paint_font_ok,
                            score.horizontal_containment,
                            score.cjk_extract_ratio
                        );
                        ExitCode::from(1)
                    } else {
                        ExitCode::SUCCESS
                    }
                }
                Err(err) => {
                    eprintln!("rpt-bench: {err}");
                    ExitCode::from(1)
                }
            }
        }
        Command::Corpus {
            manifest,
            cache,
            out,
            render_pages,
            max_pages,
        } => match score_corpus(&manifest, &cache, &out, render_pages, max_pages) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("rpt-bench: {err}");
                ExitCode::from(1)
            }
        },
    }
}

fn score_corpus(
    manifest_path: &Path,
    cache: &Path,
    out: &Path,
    render_pages: u32,
    max_pages: Option<u32>,
) -> Result<(), String> {
    let manifest: Manifest = serde_json::from_str(
        &fs::read_to_string(manifest_path).map_err(|err| format!("read manifest: {err}"))?,
    )
    .map_err(|err| format!("parse manifest: {err}"))?;
    let opts = ScoreOptions {
        render_pages,
        max_pages,
    };
    let mut files = Vec::new();
    for doc in &manifest.documents {
        let path = cache.join(format!("{}.pdf", doc.id));
        eprint!("bench {} ", doc.id);
        if !path.exists() {
            eprintln!("missing");
            files.push(serde_json::json!({
                "id": doc.id,
                "status": "missing",
            }));
            continue;
        }
        match score_pair(&path, &path, &opts) {
            Ok(score) => {
                eprintln!(
                    "glyphs {} unmapped {} drop {:.3}",
                    score.source_glyphs, score.source_unmapped, score.drop_rate
                );
                files.push(serde_json::json!({
                    "id": doc.id,
                    "status": "ok",
                    "score": score,
                }));
            }
            Err(err) => {
                eprintln!("error {err}");
                files.push(serde_json::json!({
                    "id": doc.id,
                    "status": "error",
                    "error": err,
                }));
            }
        }
    }
    let payload = serde_json::json!({
        "kind": "identity-ceiling",
        "note": "Each file is scored against itself. drop_rate and non-text SSIM should be ideal. source_unmapped_ratio is the extraction gap.",
        "files": files,
    });
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    fs::write(
        out,
        serde_json::to_string_pretty(&payload).map_err(|err| err.to_string())?,
    )
    .map_err(|err| err.to_string())?;
    eprintln!("wrote {}", out.display());
    Ok(())
}
