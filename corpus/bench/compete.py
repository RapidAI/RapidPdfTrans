#!/usr/bin/env python3
"""Head-to-head layout scores for one shared identity translator.

RapidPdfTrans, BabelDOC, and PDFMathTranslate (pdf2zh_next) all talk to
``corpus/bench/identity_server.py``. The translator returns the source text,
so drop rate, overflow, style, formula integrity, and non-text SSIM compare
layout rather than model quality.

    python3 corpus/bench/identity_server.py --port 8765
    python3 corpus/bench/compete.py --pages 1

``--live`` uses the shared gateway (``RPT_LLM_BASE_URL``, model ``auto``)
and writes ``corpus/benchmarks/live-head-to-head.json`` instead of the
identity record. The API key is read from the environment and scrubbed
from logs and reports.

BabelDOC and pdf2zh_next must be on PATH. ``rpt`` is taken from
``target/release`` or ``target/debug``. The identity server must already
be listening. RapidPdfTrans still reads ``RPT_LLM_API_KEY`` from the
environment; the key is not written into the report.
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
ENGINES = ("rapidpdftrans", "babeldoc", "pdf2zh_next")
METRICS = (
    "drop_rate",
    "line_coverage",
    "protected_recall",
    "formula_integrity",
    "overflow_rate",
    "style_retention",
    "identity_char_retention",
    "horizontal_containment",
    "mean_nontext_ssim",
    "reference_byte_identity",
)


def find_bin(name: str) -> Path | None:
    if name in ("rpt", "rpt-bench"):
        found = [
            path
            for kind in ("release", "debug")
            if (path := ROOT / "target" / kind / name).exists()
        ]
        if not found:
            return None
        return max(found, key=lambda path: path.stat().st_mtime)
    found = shutil.which(name)
    return Path(found) if found else None


def git_head() -> str:
    try:
        return subprocess.check_output(
            ["git", "rev-parse", "--short", "HEAD"], cwd=ROOT, text=True
        ).strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return "unknown"


def load_docs(manifest: Path) -> list[dict]:
    payload = json.loads(manifest.read_text())
    return payload["documents"]


def pick_pdf(directory: Path) -> Path | None:
    ranked: list[tuple[int, float, Path]] = []
    for path in directory.rglob("*.pdf"):
        name = path.name.lower()
        if "dual" in name:
            continue
        score = 0
        if "mono" in name:
            score += 2
        if "watermark" not in name:
            score += 1
        ranked.append((score, path.stat().st_mtime, path))
    if not ranked:
        return None
    ranked.sort()
    return ranked[-1][2]


def scrub(text: str) -> str:
    """Drop the gateway key before a log or a report is written."""
    secret = os.environ.get("RPT_LLM_API_KEY", "")
    if secret:
        text = text.replace(secret, "[redacted]")
    return text


def run_cmd(cmd: list[str], cwd: Path, log: Path, timeout: int) -> tuple[int, str]:
    env = os.environ.copy()
    env.setdefault("PYTHONUNBUFFERED", "1")
    try:
        completed = subprocess.run(
            cmd,
            cwd=cwd,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired as exc:
        output = exc.stdout or ""
        if isinstance(output, bytes):
            output = output.decode("utf-8", "replace")
        log.write_text(scrub(output + f"\nTIMEOUT after {timeout}s\n"))
        return 124, f"timeout after {timeout}s"
    log.write_text(scrub(completed.stdout or ""))
    if completed.returncode != 0:
        tail = scrub((completed.stdout or "")[-1500:])
        return completed.returncode, tail
    return 0, ""


def translate_rpt(
    rpt: Path,
    source: Path,
    out_dir: Path,
    pages: int,
    timeout: int,
    *,
    base_url: str,
    model: str,
    batch_size: int,
    jobs: int,
) -> tuple[Path | None, str]:
    out_dir.mkdir(parents=True, exist_ok=True)
    output = out_dir / "translated.pdf"
    cmd = [
        str(rpt),
        "translate",
        str(source),
        "--output",
        str(output),
        "--max-pages",
        str(pages),
        "--from",
        "en",
        "--to",
        "zh",
        "--base-url",
        base_url,
        "--model",
        model,
        "--batch-size",
        str(batch_size),
        "--jobs",
        str(jobs),
    ]
    code, detail = run_cmd(cmd, out_dir, out_dir / "rpt.log", timeout)
    # Coverage can fail after the PDF is saved. Score that file.
    if output.exists() and output.stat().st_size > 0:
        return output, ""
    return None, detail or f"rpt exit {code}"


def translate_babeldoc(
    binary: Path,
    source: Path,
    out_dir: Path,
    pages: int,
    timeout: int,
    *,
    base_url: str,
    model: str,
    api_key: str,
    qps: int,
) -> tuple[Path | None, str]:
    out_dir.mkdir(parents=True, exist_ok=True)
    cmd = [
        str(binary),
        "--openai",
        "--openai-model",
        model,
        "--openai-base-url",
        base_url,
        "--openai-api-key",
        api_key,
        "--lang-in",
        "en",
        "--lang-out",
        "zh",
        "--no-dual",
        "--no-auto-extract-glossary",
        "--disable-same-text-fallback",
        "--watermark-output-mode",
        "no_watermark",
        "--only-include-translated-page",
        "--pages",
        f"1-{pages}",
        "--qps",
        str(qps),
        "--primary-font-family",
        "serif",
        "--output",
        str(out_dir),
        "--working-dir",
        str(out_dir / "work"),
        "--files",
        str(source),
    ]
    code, detail = run_cmd(cmd, out_dir, out_dir / "babeldoc.log", timeout)
    pdf = pick_pdf(out_dir)
    if pdf is None:
        return None, detail or f"babeldoc exit {code}"
    return pdf, ""


def translate_pdf2zh(
    binary: Path,
    source: Path,
    out_dir: Path,
    pages: int,
    timeout: int,
    *,
    base_url: str,
    model: str,
    api_key: str,
    qps: int,
) -> tuple[Path | None, str]:
    out_dir.mkdir(parents=True, exist_ok=True)
    cmd = [
        str(binary),
        str(source),
        "--openai",
        "--openai-model",
        model,
        "--openai-base-url",
        base_url,
        "--openai-api-key",
        api_key,
        "--lang-in",
        "en",
        "--lang-out",
        "zh",
        "--no-dual",
        "--no-auto-extract-glossary",
        "--watermark-output-mode",
        "no_watermark",
        "--only-include-translated-page",
        "--pages",
        f"1-{pages}",
        "--qps",
        str(qps),
        "--primary-font-family",
        "serif",
        "--output",
        str(out_dir),
    ]
    code, detail = run_cmd(cmd, out_dir, out_dir / "pdf2zh.log", timeout)
    pdf = pick_pdf(out_dir)
    if pdf is None:
        return None, detail or f"pdf2zh_next exit {code}"
    return pdf, ""


def score(bench: Path, source: Path, output: Path, pages: int) -> dict:
    completed = subprocess.run(
        [
            str(bench),
            "pair",
            "--source",
            str(source),
            "--output",
            str(output),
            "--render-pages",
            "1",
            "--max-pages",
            str(pages),
        ],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    text = completed.stdout.strip()
    start = text.find("{")
    if start < 0:
        raise RuntimeError(completed.stderr[-800:] or "rpt-bench produced no JSON")
    return json.loads(text[start:])


def mean(rows: list[dict], key: str) -> float | None:
    values = []
    for row in rows:
        value = (row.get("score") or {}).get(key)
        if isinstance(value, (int, float)):
            values.append(float(value))
    if not values:
        return None
    return sum(values) / len(values)


def fmt(value) -> str:
    if value is None:
        return "n/a"
    return f"{float(value):.4f}"


def markdown(payload: dict) -> str:
    lines = [
        "# Head-to-head translation benchmark",
        "",
        f"Recorded {payload['recorded_at']} on commit `{payload['git']}`.",
        "",
        "Each engine translated the same CI pages through "
        f"`{payload['translator']}`, limited to the first {payload['pages']} page(s). "
        + (
            "The model is the shared gateway (`auto`). "
            "`identity_char_retention` near 1.0 means the source characters survived; "
            "a real translation is expected to score lower there."
            if payload.get("kind") == "live-head-to-head"
            else "The translator returns the source text, so these numbers measure layout "
            "damage rather than translation quality. `identity_char_retention` near 1.0 "
            "means the source characters survived."
        ),
        "",
        "BabelDOC is 0.6.x (`babeldoc`). PDFMathTranslate is pdf2zh_next 2.9.0. "
        "Both use `--primary-font-family serif`, `--no-dual`, and no watermark. "
        "BabelDOC is also passed `--disable-same-text-fallback` so an identity "
        "reply is kept. pdf2zh_next has no such flag; identical paragraphs fall "
        "back to its one-line translator, which still echoes the source.",
        "",
        "## Means",
        "",
        "| engine | docs | drop | coverage | protected | formula | overflow | style | identity | containment | non-text SSIM |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for engine in ENGINES:
        rows = [row for row in payload["files"] if row["engine"] == engine and row.get("score")]
        summary = payload["summary"].get(engine) or {}
        lines.append(
            "| `{engine}` | {docs} | {drop} | {cov} | {prot} | {formula} | {overflow} | {style} | {ident} | {contain} | {ssim} |".format(
                engine=engine,
                docs=summary.get("documents", len(rows)),
                drop=fmt(summary.get("drop_rate")),
                cov=fmt(summary.get("line_coverage")),
                prot=fmt(summary.get("protected_recall")),
                formula=fmt(summary.get("formula_integrity")),
                overflow=fmt(summary.get("overflow_rate")),
                style=fmt(summary.get("style_retention")),
                ident=fmt(summary.get("identity_char_retention")),
                contain=fmt(summary.get("horizontal_containment")),
                ssim=fmt(summary.get("mean_nontext_ssim")),
            )
        )
    lines += [
        "",
        "## Per file",
        "",
        "| id | engine | status | drop | formula | overflow | style | identity | containment |",
        "| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for row in payload["files"]:
        score_row = row.get("score") or {}
        lines.append(
            "| `{id}` | `{engine}` | {status} | {drop} | {formula} | {overflow} | {style} | {ident} | {contain} |".format(
                id=row["id"],
                engine=row["engine"],
                status=row["status"],
                drop=fmt(score_row.get("drop_rate")),
                formula=fmt(score_row.get("formula_integrity")),
                overflow=fmt(score_row.get("overflow_rate")),
                style=fmt(score_row.get("style_retention")),
                ident=fmt(score_row.get("identity_char_retention")),
                contain=fmt(score_row.get("horizontal_containment")),
            )
        )
    failures = [row for row in payload["files"] if row["status"] != "ok"]
    if failures:
        lines += ["", "## Failures", ""]
        for row in failures:
            detail = (row.get("detail") or "").replace("\n", " ")[:400]
            lines.append(f"- `{row['id']}` / `{row['engine']}`: {detail}")
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", default="corpus/ci/manifest.json")
    parser.add_argument("--pages", type=int, default=1)
    parser.add_argument("--timeout", type=int, default=1200)
    parser.add_argument("--work", default="/tmp/rpt-compete")
    parser.add_argument("--engines", default=",".join(ENGINES))
    parser.add_argument(
        "--live",
        action="store_true",
        help="translate through the shared gateway (model auto) instead of the identity server",
    )
    parser.add_argument(
        "--ids",
        default="",
        help="comma-separated corpus ids; live defaults to three CI papers",
    )
    args = parser.parse_args()
    engines = [name for name in args.engines.split(",") if name]
    rpt = find_bin("rpt")
    bench = find_bin("rpt-bench")
    babeldoc = find_bin("babeldoc")
    pdf2zh = find_bin("pdf2zh_next")
    if bench is None or rpt is None:
        print("build rpt and rpt-bench first", file=sys.stderr)
        return 2
    missing = []
    if "babeldoc" in engines and babeldoc is None:
        missing.append("babeldoc")
    if "pdf2zh_next" in engines and pdf2zh is None:
        missing.append("pdf2zh_next")
    if missing:
        print("engines not on PATH:", ", ".join(missing), file=sys.stderr)
        return 2
    if "rapidpdftrans" in engines and not os.environ.get("RPT_LLM_API_KEY"):
        print("RPT_LLM_API_KEY is required for rpt translate", file=sys.stderr)
        return 2

    live = args.live
    base_url = (
        os.environ.get("RPT_LLM_BASE_URL", "https://hub.mypapers.top/api/llm/v1")
        if live
        else "http://127.0.0.1:8765/v1"
    )
    model = "auto" if live else "identity"
    api_key = os.environ.get("RPT_LLM_API_KEY", "") if live else "local"
    batch_size = 1 if live else 16
    jobs = 1 if live else 1
    qps = 2 if live else 16
    if live and not api_key:
        print("RPT_LLM_API_KEY is required for a live run", file=sys.stderr)
        return 2

    manifest = ROOT / args.manifest
    docs = load_docs(manifest)
    wanted = [item.strip() for item in args.ids.split(",") if item.strip()]
    if live and not wanted:
        wanted = [
            "arxiv-2610.02163",
            "pmlr-v202-abbas23a",
            "neurips-2023-00296c0e",
        ]
    if wanted:
        by_id = {doc["id"]: doc for doc in docs}
        missing = [item for item in wanted if item not in by_id]
        if missing:
            print("unknown ids:", ", ".join(missing), file=sys.stderr)
            return 2
        docs = [by_id[item] for item in wanted]
    work = Path(args.work)
    work.mkdir(parents=True, exist_ok=True)
    rows: list[dict] = []
    for doc in docs:
        source = ROOT / "corpus" / "ci" / f"{doc['id']}.pdf"
        if not source.exists():
            for engine in engines:
                rows.append(
                    {
                        "id": doc["id"],
                        "engine": engine,
                        "status": "missing_pdf",
                        "detail": str(source),
                    }
                )
            continue
        for engine in engines:
            out_dir = work / doc["id"] / engine
            print(f"== {engine} {doc['id']}", flush=True)
            try:
                if engine == "rapidpdftrans":
                    pdf, detail = translate_rpt(
                        rpt,
                        source,
                        out_dir,
                        args.pages,
                        args.timeout,
                        base_url=base_url,
                        model=model,
                        batch_size=batch_size,
                        jobs=jobs,
                    )
                elif engine == "babeldoc":
                    pdf, detail = translate_babeldoc(
                        babeldoc,
                        source,
                        out_dir,
                        args.pages,
                        args.timeout,
                        base_url=base_url,
                        model=model,
                        api_key=api_key,
                        qps=qps,
                    )
                elif engine == "pdf2zh_next":
                    pdf, detail = translate_pdf2zh(
                        pdf2zh,
                        source,
                        out_dir,
                        args.pages,
                        args.timeout,
                        base_url=base_url,
                        model=model,
                        api_key=api_key,
                        qps=qps,
                    )
                else:
                    pdf, detail = None, f"unknown engine {engine}"
            except Exception as exc:  # noqa: BLE001
                pdf, detail = None, str(exc)
            detail = scrub(detail or "")
            if pdf is None:
                print(f"   failed: {detail[-200:]}", flush=True)
                rows.append(
                    {
                        "id": doc["id"],
                        "engine": engine,
                        "status": "failed",
                        "detail": detail[-1500:],
                    }
                )
                continue
            try:
                scored = score(bench, source, pdf, args.pages)
            except Exception as exc:  # noqa: BLE001
                rows.append(
                    {
                        "id": doc["id"],
                        "engine": engine,
                        "status": "score_failed",
                        "output": str(pdf),
                        "detail": str(exc),
                    }
                )
                continue
            print(
                "   drop={drop:.3f} overflow={overflow:.3f} style={style:.3f} identity={ident:.3f} contain={contain:.3f}".format(
                    drop=scored.get("drop_rate") or 0,
                    overflow=scored.get("overflow_rate") or 0,
                    style=scored.get("style_retention") or 0,
                    ident=scored.get("identity_char_retention") or 0,
                    contain=scored.get("horizontal_containment") or 0,
                ),
                flush=True,
            )
            rows.append(
                {
                    "id": doc["id"],
                    "engine": engine,
                    "status": "ok",
                    "output": str(pdf),
                    "score": scored,
                }
            )

    summary = {}
    for engine in engines:
        ok = [row for row in rows if row["engine"] == engine and row.get("score")]
        summary[engine] = {"documents": len(ok)}
        for key in METRICS:
            summary[engine][key] = mean(ok, key)
    payload = {
        "recorded_at": dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "git": git_head(),
        "kind": "live-head-to-head" if live else "head-to-head",
        "translator": f"gateway {base_url} model {model}" if live else "identity http://127.0.0.1:8765/v1",
        "pages": args.pages,
        "engines": engines,
        "versions": {
            "rpt": str(rpt),
            "babeldoc": str(babeldoc) if babeldoc else None,
            "pdf2zh_next": str(pdf2zh) if pdf2zh else None,
        },
        "summary": summary,
        "files": rows,
    }
    bench_dir = ROOT / "corpus" / "benchmarks"
    bench_dir.mkdir(parents=True, exist_ok=True)
    stamp = payload["recorded_at"].replace(":", "").replace("-", "")
    suffix = "live" if live else "engines"
    run_path = bench_dir / "runs" / f"{stamp}-{suffix}.json"
    run_path.parent.mkdir(parents=True, exist_ok=True)
    text = scrub(json.dumps(payload, indent=2) + "\n")
    report = scrub(markdown(payload))
    run_path.write_text(text)
    if live:
        (bench_dir / "live-head-to-head.json").write_text(text)
        (bench_dir / "live-head-to-head.md").write_text(report)
    else:
        (bench_dir / "head-to-head.json").write_text(text)
        (bench_dir / "head-to-head.md").write_text(report)
        (bench_dir / "latest.md").write_text(report)
    history = {
        "recorded_at": payload["recorded_at"],
        "git": payload["git"],
        "kind": payload["kind"],
        "pages": args.pages,
        "summary": summary,
    }
    with (bench_dir / "history.jsonl").open("a") as handle:
        handle.write(json.dumps(history) + "\n")
    print(f"wrote {run_path}")
    return 0 if any(row.get("score") for row in rows) else 1


if __name__ == "__main__":
    raise SystemExit(main())
