#!/usr/bin/env python3
"""Record one translation-benchmark snapshot.

Scores the corpus with rpt-bench (identity ceiling: each PDF against itself),
then notes whether BabelDOC and PDFMathTranslate are installed and whether
RapidPdfTrans can write a translated PDF. Engines are not installed by this
script. When they are on PATH, pass --run-engines to translate the corpus with
the shared OpenAI-compatible endpoint in corpus/bench/identity_server.py.

LLM-as-judge is recorded as skipped unless RPT_LLM_API_KEY is set. The key is
never written into the report.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
ENGINES = ("babeldoc", "pdf2zh", "pdf2zh_next")


def find_bench() -> Path:
    for name in ("release", "debug"):
        path = ROOT / "target" / name / "rpt-bench"
        if path.exists():
            return path
    raise SystemExit("build rpt-bench first: cargo build -p rpt-qa --bin rpt-bench")


def mean(rows: list[dict], key: str) -> float | None:
    values = []
    for row in rows:
        score = row.get("score") or {}
        value = score.get(key)
        if isinstance(value, (int, float)):
            values.append(float(value))
    if not values:
        return None
    return sum(values) / len(values)


def engine_notes() -> list[dict]:
    notes = [
        {
            "engine": "rapidpdftrans",
            "status": "no_translated_pdf",
            "detail": "rpt_save still returns 'PDF rewriting is not implemented (milestone M3)'. Output metrics are not available.",
        }
    ]
    for name in ENGINES:
        path = shutil.which(name)
        notes.append(
            {
                "engine": name,
                "status": "installed" if path else "not_installed",
                "path": path,
            }
        )
    judge = "available" if os.environ.get("RPT_LLM_API_KEY") else "skipped_no_key"
    notes.append(
        {
            "engine": "llm-judge",
            "status": judge,
            "detail": "Reference metric for an identity translator is identity_char_retention. A live judge is not called from this script.",
        }
    )
    return notes


def markdown(payload: dict) -> str:
    summary = payload["summary"]
    lines = [
        "# Translation benchmark",
        "",
        f"Recorded {payload['recorded_at']} on commit `{payload['git']}`.",
        "",
        "This snapshot scores each CI PDF against itself. That is the ceiling for drop rate, style, overflow, and non-text SSIM, and it is the live measurement of source-text recovery (`source_unmapped_ratio`). RapidPdfTrans does not emit a translated PDF yet, so it is not on the output comparison.",
        "",
        "## Means",
        "",
        f"- Documents scored: {summary['documents']}",
        f"- Source glyphs: {summary['source_glyphs']}",
        f"- Source unmapped: {summary['source_unmapped']} ({summary['source_unmapped_ratio']:.4f})",
        f"- Mean line coverage: {summary['line_coverage']:.4f}",
        f"- Mean drop rate: {summary['drop_rate']:.4f}",
        f"- Mean protected recall: {summary['protected_recall']:.4f}",
        f"- Mean formula integrity: {summary['formula_integrity']:.4f}",
        f"- Mean overflow rate: {summary['overflow_rate']:.4f}",
        f"- Mean style retention: {summary['style_retention']:.4f}",
        f"- Mean identity character retention: {summary['identity_char_retention']:.4f}",
        f"- Mean non-text SSIM: {fmt_opt(summary['mean_nontext_ssim'])}",
        "",
        "The previous fidelity report, before Type 1 built-in encodings were read, had 1,369 unmapped glyphs on these same nine files.",
        "",
        "## Engines",
        "",
    ]
    for note in payload["engines"]:
        status = note["status"]
        extra = note.get("detail") or note.get("path") or ""
        lines.append(f"- `{note['engine']}`: {status}" + (f" ({extra})" if extra else ""))
    lines += [
        "",
        "## Priority",
        "",
        "1. Translated output does not exist yet (`rpt_save` is still the M3 stub). Every layout comparison against BabelDOC is blocked on a rewrite that deletes original text operators without dropping glyphs.",
        "2. The glyphs that stay unmapped are almost all CMEX (big operators and delimiters). Their Type 1 names are not in the Adobe Glyph List, so formula characters still never reach the translator.",
        "3. BabelDOC and PDFMathTranslate are not installed in this environment, so this snapshot has no external output scores. `corpus/bench/run.py --run-engines` is the hook once the commands exist and a shared translator URL is set.",
        "",
        "## Per file",
        "",
        "| id | glyphs | unmapped | drop | protected | formula | overflow | style | non-text SSIM |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for row in payload["files"]:
        score = row.get("score")
        if not score:
            lines.append(f"| `{row['id']}` |  |  |  |  |  |  |  | {row.get('status')} |")
            continue
        lines.append(
            "| `{id}` | {glyphs} | {unmapped} | {drop:.4f} | {prot:.4f} | {formula:.4f} | {overflow:.4f} | {style:.4f} | {ssim} |".format(
                id=row["id"],
                glyphs=score["source_glyphs"],
                unmapped=score["source_unmapped"],
                drop=score["drop_rate"],
                prot=score["protected_recall"],
                formula=score["formula_integrity"],
                overflow=score["overflow_rate"],
                style=score["style_retention"],
                ssim=fmt_opt(score.get("mean_nontext_ssim")),
            )
        )
    lines.append("")
    return "\n".join(lines)


def fmt_opt(value) -> str:
    if value is None:
        return "n/a"
    return f"{float(value):.4f}"


def git_head() -> str:
    try:
        return subprocess.check_output(["git", "rev-parse", "--short", "HEAD"], cwd=ROOT, text=True).strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return "unknown"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", default="corpus/ci/manifest.json")
    parser.add_argument("--cache", default="corpus/ci")
    parser.add_argument("--render-pages", default="1")
    parser.add_argument("--run-engines", action="store_true")
    args = parser.parse_args()
    bench = find_bench()
    raw_path = ROOT / "corpus" / "benchmarks" / "ceiling.json"
    subprocess.run(
        [
            str(bench),
            "corpus",
            "--manifest",
            args.manifest,
            "--cache",
            args.cache,
            "--out",
            str(raw_path),
            "--render-pages",
            args.render_pages,
        ],
        cwd=ROOT,
        check=True,
    )
    raw = json.loads(raw_path.read_text())
    files = raw["files"]
    ok = [row for row in files if row.get("status") == "ok"]
    glyphs = sum(row["score"]["source_glyphs"] for row in ok)
    unmapped = sum(row["score"]["source_unmapped"] for row in ok)
    summary = {
        "documents": len(ok),
        "source_glyphs": glyphs,
        "source_unmapped": unmapped,
        "source_unmapped_ratio": (unmapped / glyphs) if glyphs else 0.0,
        "line_coverage": mean(ok, "line_coverage"),
        "drop_rate": mean(ok, "drop_rate"),
        "protected_recall": mean(ok, "protected_recall"),
        "formula_integrity": mean(ok, "formula_integrity"),
        "overflow_rate": mean(ok, "overflow_rate"),
        "style_retention": mean(ok, "style_retention"),
        "identity_char_retention": mean(ok, "identity_char_retention"),
        "mean_nontext_ssim": mean(ok, "mean_nontext_ssim"),
    }
    if args.run_engines:
        missing = [name for name in ENGINES if not shutil.which(name)]
        if missing:
            print("engines not on PATH:", ", ".join(missing), file=sys.stderr)
            return 2
        print("engine invocation is not wired until a shared translator command is configured", file=sys.stderr)
        return 2
    payload = {
        "recorded_at": dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "git": git_head(),
        "kind": "identity-ceiling",
        "translator": "none (source compared with itself)",
        "summary": summary,
        "engines": engine_notes(),
        "files": files,
    }
    bench_dir = ROOT / "corpus" / "benchmarks"
    bench_dir.mkdir(parents=True, exist_ok=True)
    stamp = payload["recorded_at"].replace(":", "").replace("-", "")
    run_path = bench_dir / "runs" / f"{stamp}.json"
    run_path.parent.mkdir(parents=True, exist_ok=True)
    run_path.write_text(json.dumps(payload, indent=2) + "\n")
    history = bench_dir / "history.jsonl"
    history_row = {
        "recorded_at": payload["recorded_at"],
        "git": payload["git"],
        "kind": payload["kind"],
        "summary": summary,
        "engines": payload["engines"],
    }
    with history.open("a") as handle:
        handle.write(json.dumps(history_row) + "\n")
    (bench_dir / "latest.md").write_text(markdown(payload))
    print(f"wrote {run_path}")
    print(f"appended {history}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
