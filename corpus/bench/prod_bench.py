#!/usr/bin/env python3
"""Wall time, peak RSS, output size, and coverage for three translators.

Compares rpt 0.1.4, rpt 0.1.5, and BabelDOC on the same PDFs, the same
OpenAI-compatible gateway, and the same concurrency. The gateway comes from
the environment (``RPT_LLM_API_KEY``, ``RPT_LLM_BASE_URL``, ``RPT_LLM_MODEL``).
The key is never written into the report.

Default documents:

* ``corpus/ci/arxiv-2610.02163.pdf`` (11 pages)
* ``corpus/ci/arxiv-2610.03665.pdf`` (16 pages)
* a generated 30-page letter PDF, about 45 MB, with one uncompressed
  800×600 RGB image and 40 Helvetica lines on every page

Concurrency is ``--jobs`` (default 4): rpt ``--jobs``, BabelDOC ``--qps`` and
``--pool-max-workers``. BabelDOC also gets ``--ignore-cache``, ``--no-dual``,
``--no-auto-extract-glossary``, and no watermark, so the timed file is one
monolingual PDF. rpt uses its default replace mode and default batch size (8).

Peak RSS is GNU ``/usr/bin/time -v`` "Maximum resident set size". Coverage for
every engine is ``rpt-bench pair --render-pages 0`` line coverage: a source
text line whose box still meets an output glyph. rpt's own coverage object is
recorded beside that. 0.1.4 counts every intentional keep as unresolved; 0.1.5
counts only a real miss.

    python3 corpus/bench/prod_bench.py \\
        --rpt-014 /tmp/rpt-014/target/release/rpt \\
        --rpt-015 target/release/rpt \\
        --rpt-bench target/release/rpt-bench \\
        --babeldoc .venv/bin/babeldoc \\
        --jobs 4 \\
        --work /tmp/rpt-prod-bench
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_PAPERS = (
    ROOT / "corpus" / "ci" / "arxiv-2610.02163.pdf",
    ROOT / "corpus" / "ci" / "arxiv-2610.03665.pdf",
)


def scrub(text: str) -> str:
    secret = os.environ.get("RPT_LLM_API_KEY", "")
    if secret:
        text = text.replace(secret, "[redacted]")
    return text


def generate_large(path: Path) -> None:
    """30-page letter PDF. Each page has its own uncompressed 800×600 image."""
    page_count = 30
    width, height = 800, 600
    raw_len = width * height * 3
    # 1 catalog, 2 pages, 3 font, then page/content/image triples.
    objs: dict[int, bytes] = {}
    kids = " ".join(f"{4 + i * 3} 0 R" for i in range(page_count))
    objs[1] = b"<< /Type /Catalog /Pages 2 0 R >>"
    objs[2] = f"<< /Type /Pages /Count {page_count} /Kids [{kids}] >>".encode()
    objs[3] = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"
    for index in range(page_count):
        page_id = 4 + index * 3
        content_id = page_id + 1
        image_id = page_id + 2
        lines = []
        for line_no in range(40):
            lines.append(
                "The quick brown fox jumps over the lazy dog and then explains "
                f"the method in detail. Line {line_no} continues with ordinary "
                "body text about the experimental setup."
            )
        commands = ["BT", "/F1 10 Tf", "72 760 Td"]
        for nth, line in enumerate(lines):
            if nth:
                commands.append("0 -16 Td")
            escaped = line.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")
            commands.append(f"({escaped}) Tj")
        commands.append("ET")
        commands.append("q 400 0 0 300 72 40 cm /Im0 Do Q")
        stream = ("\n".join(commands) + "\n").encode()
        objs[content_id] = (
            f"<< /Length {len(stream)} >>\nstream\n".encode() + stream + b"endstream"
        )
        raw = _image(index, raw_len)
        objs[image_id] = (
            (
                f"<< /Type /XObject /Subtype /Image /Width {width} /Height {height} "
                f"/ColorSpace /DeviceRGB /BitsPerComponent 8 /Length {raw_len} >>\nstream\n"
            ).encode()
            + raw
            + b"\nendstream"
        )
        objs[page_id] = (
            f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            f"/Resources << /Font << /F1 3 0 R >> /XObject << /Im0 {image_id} 0 R >> >> "
            f"/Contents {content_id} 0 R >>"
        ).encode()
    _write_pdf(path, objs)


def _image(page: int, raw_len: int) -> bytes:
    # Uncompressed in the PDF, so a short unique block repeated still occupies
    # width*height*3 bytes. The block changes with the page, so each XObject
    # is its own stream.
    x = (0x9E3779B9 ^ (page * 0x85EBCA6B)) & 0xFFFFFFFF
    block = bytearray(4096)
    for i in range(4096):
        x = (x * 1664525 + 1013904223) & 0xFFFFFFFF
        block[i] = (x >> 16) & 0xFF
    repeats, rest = divmod(raw_len, 4096)
    return bytes(block) * repeats + bytes(block[:rest])


def _write_pdf(path: Path, objs: dict[int, bytes]) -> None:
    out = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
    offsets = [0] * (max(objs) + 1)
    for number in range(1, max(objs) + 1):
        offsets[number] = len(out)
        body = objs[number]
        out += f"{number} 0 obj\n".encode()
        out += body
        if not body.endswith(b"\n"):
            out += b"\n"
        out += b"endobj\n"
    xref = len(out)
    count = max(objs) + 1
    out += f"xref\n0 {count}\n".encode()
    out += b"0000000000 65535 f \n"
    for number in range(1, count):
        out += f"{offsets[number]:010d} 00000 n \n".encode()
    out += (
        f"trailer\n<< /Size {count} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
    )
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(out)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def parse_time(text: str) -> tuple[float | None, int | None]:
    elapsed = None
    rss = None
    match = re.search(r"Elapsed \(wall clock\) time \(h:mm:ss or m:ss\): ([0-9:.]+)", text)
    if match:
        parts = match.group(1).split(":")
        if len(parts) == 3:
            elapsed = int(parts[0]) * 3600 + int(parts[1]) * 60 + float(parts[2])
        elif len(parts) == 2:
            elapsed = int(parts[0]) * 60 + float(parts[1])
    match = re.search(r"Maximum resident set size \(kbytes\): (\d+)", text)
    if match:
        rss = int(match.group(1))
    return elapsed, rss


def coverage_from_stdout(path: Path) -> dict | None:
    if not path.exists() or path.stat().st_size == 0:
        return None
    raw = path.read_bytes()
    start = raw.find(b"{")
    if start < 0:
        return None
    # Glyph JSON is large. The coverage object is small; pull it out by braces
    # so a 200 MB stdout does not have to become a Python dict.
    key = raw.find(b'"coverage"', start)
    if key < 0:
        return None
    brace = raw.find(b"{", key)
    if brace < 0:
        return None
    depth = 0
    in_string = False
    escape = False
    for index in range(brace, len(raw)):
        byte = raw[index]
        if in_string:
            if escape:
                escape = False
            elif byte == ord("\\"):
                escape = True
            elif byte == ord('"'):
                in_string = False
            continue
        if byte == ord('"'):
            in_string = True
        elif byte == ord("{"):
            depth += 1
        elif byte == ord("}"):
            depth -= 1
            if depth == 0:
                try:
                    return json.loads(raw[brace : index + 1])
                except json.JSONDecodeError:
                    return None
    return None


def tool_coverage(report: dict | None) -> str:
    if not report:
        return "n/a"
    unresolved = report.get("unresolved_count")
    if unresolved is None and isinstance(report.get("unresolved_ids"), list):
        unresolved = len(report["unresolved_ids"])
    real = report.get("real_misses")
    intentional = report.get("intentional")
    complete = report.get("complete")
    state = "complete" if complete else "incomplete"
    parts = [state]
    if real is not None:
        parts.append(f"{real} real misses")
    elif unresolved is not None:
        parts.append(f"{unresolved} unresolved")
    if intentional is not None:
        parts.append(f"{intentional} intentional")
    return ", ".join(parts)


def pick_pdf(directory: Path) -> Path | None:
    ranked: list[tuple[int, float, Path]] = []
    for path in directory.rglob("*.pdf"):
        if "work" in path.parts:
            continue
        name = path.name.lower()
        if "dual" in name:
            continue
        score = 0
        if "mono" in name:
            score += 2
        if "watermark" not in name or "no_watermark" in name:
            score += 1
        ranked.append((score, path.stat().st_mtime, path))
    if not ranked:
        return None
    ranked.sort()
    return ranked[-1][2]


def run_timed(cmd: list[str], cwd: Path, stdout: Path, stderr: Path, time_path: Path, timeout: int) -> int:
    env = os.environ.copy()
    env.setdefault("PYTHONUNBUFFERED", "1")
    # A checkpoint or an external font would make one engine not match the others.
    for name in (
        "RPT_TRANSLATE_CHECKPOINT",
        "RPT_CORPUS_DIR",
        "RPT_FONT_DIR",
        "RPT_CJK_FONT",
        "RPT_TRANSLATOR",
        "RPT_GOOGLE_API_KEY",
    ):
        env.pop(name, None)
    timed = ["/usr/bin/time", "-v", "-o", str(time_path), "--", *cmd]
    with stdout.open("wb") as out, stderr.open("wb") as err:
        try:
            completed = subprocess.run(
                timed,
                cwd=cwd,
                env=env,
                stdout=out,
                stderr=err,
                timeout=timeout,
            )
        except subprocess.TimeoutExpired:
            err.write(f"\nTIMEOUT after {timeout}s\n".encode())
            return 124
    return completed.returncode


def score_line_coverage(bench: Path, source: Path, output: Path, log: Path) -> float | None:
    completed = subprocess.run(
        [
            str(bench),
            "pair",
            "--source",
            str(source),
            "--output",
            str(output),
            "--render-pages",
            "0",
        ],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    log.write_text(scrub((completed.stdout or "") + "\n" + (completed.stderr or "")))
    start = (completed.stdout or "").find("{")
    if start < 0:
        return None
    try:
        payload = json.loads(completed.stdout[start:])
    except json.JSONDecodeError:
        return None
    value = payload.get("line_coverage")
    if isinstance(value, (int, float)):
        return float(value)
    return None


def load_done(path: Path) -> dict[tuple[str, str], dict]:
    done: dict[tuple[str, str], dict] = {}
    if not path.exists():
        return done
    for line in path.read_text().splitlines():
        if not line.strip():
            continue
        row = json.loads(line)
        done[(row["pdf"], row["engine"])] = row
    return done


def fmt_seconds(value: float | None) -> str:
    if value is None:
        return "n/a"
    if value >= 3600:
        minutes, seconds = divmod(int(round(value)), 60)
        hours, minutes = divmod(minutes, 60)
        return f"{hours}h{minutes:02d}m{seconds:02d}s"
    return f"{value:.1f}s"


def fmt_mb(kbytes: int | None) -> str:
    if kbytes is None:
        return "n/a"
    return f"{kbytes / 1024:.1f} MB"


def fmt_size(nbytes: int | None) -> str:
    if nbytes is None:
        return "n/a"
    return f"{nbytes / 1048576:.2f} MB ({nbytes})"


def markdown(rows: list[dict], meta: dict) -> str:
    lines = [
        f"Recorded {meta['recorded_at']} on rpt commit `{meta['git']}`.",
        "",
        f"Gateway `{meta['base_url']}`, model `{meta['model']}`, concurrency {meta['jobs']}.",
        f"BabelDOC {meta['babeldoc_version']}. Peak RSS is `/usr/bin/time -v` maximum resident set size.",
        "Coverage is `rpt-bench` line coverage (a source line whose box still meets an output glyph).",
        "rpt's own column is its coverage object: 0.1.5 reports real misses, and keeps are intentional.",
        "",
        "| PDF | pages | source | engine | wall | peak RSS | output | coverage | rpt coverage |",
        "| --- | ---: | ---: | --- | ---: | ---: | ---: | ---: | --- |",
    ]
    for row in rows:
        coverage = "n/a" if row.get("line_coverage") is None else f"{row['line_coverage']:.4f}"
        lines.append(
            "| {pdf} | {pages} | {source} | {engine} | {wall} | {rss} | {output} | {coverage} | {own} |".format(
                pdf=row["pdf"],
                pages=row.get("pages", ""),
                source=fmt_size(row.get("source_bytes")),
                engine=row["engine"],
                wall=fmt_seconds(row.get("wall_seconds")),
                rss=fmt_mb(row.get("rss_kbytes")),
                output=fmt_size(row.get("output_bytes")),
                coverage=coverage,
                own=row.get("tool_coverage") or "n/a",
            )
        )
    failures = [row for row in rows if row.get("status") != "ok"]
    if failures:
        lines += ["", "Failures:", ""]
        for row in failures:
            detail = scrub(row.get("detail") or "").replace("\n", " ")[:300]
            lines.append(f"- `{row['pdf']}` / `{row['engine']}`: {detail}")
    lines.append("")
    return "\n".join(lines)


def git_head() -> str:
    try:
        return subprocess.check_output(
            ["git", "rev-parse", "--short", "HEAD"], cwd=ROOT, text=True
        ).strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return "unknown"


def babeldoc_version(binary: Path) -> str:
    completed = subprocess.run(
        [str(binary), "--version"],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    text = (completed.stdout or "").strip().splitlines()
    return text[-1] if text else "unknown"


def translate(
    engine: str,
    source: Path,
    out_dir: Path,
    *,
    rpt_014: Path,
    rpt_015: Path,
    babeldoc: Path,
    jobs: int,
    base_url: str,
    model: str,
    api_key: str,
    timeout: int,
) -> tuple[Path | None, int]:
    out_dir.mkdir(parents=True, exist_ok=True)
    stdout = out_dir / "stdout.txt"
    stderr = out_dir / "stderr.txt"
    time_path = out_dir / "time.txt"
    if engine == "rpt 0.1.4":
        output = out_dir / "translated.pdf"
        cmd = [
            str(rpt_014),
            "translate",
            str(source),
            "--from",
            "en",
            "--to",
            "zh",
            "--output",
            str(output),
            "--jobs",
            str(jobs),
            "--base-url",
            base_url,
            "--model",
            model,
        ]
    elif engine == "rpt 0.1.5":
        output = out_dir / "translated.pdf"
        cmd = [
            str(rpt_015),
            "translate",
            str(source),
            "--from",
            "en",
            "--to",
            "zh",
            "--output",
            str(output),
            "--jobs",
            str(jobs),
            "--base-url",
            base_url,
            "--model",
            model,
            "--progress",
            "off",
        ]
    elif engine == "babeldoc":
        output = None
        cmd = [
            str(babeldoc),
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
            "--ignore-cache",
            "--watermark-output-mode",
            "no_watermark",
            "--primary-font-family",
            "serif",
            "--qps",
            str(jobs),
            "--pool-max-workers",
            str(jobs),
            "--output",
            str(out_dir),
            "--working-dir",
            str(out_dir / "work"),
            "--files",
            str(source),
        ]
    else:
        raise SystemExit(f"unknown engine {engine}")
    (out_dir / "command.txt").write_text(scrub(" ".join(cmd) + "\n"))
    code = run_timed(cmd, out_dir, stdout, stderr, time_path, timeout)
    if engine == "babeldoc":
        output = pick_pdf(out_dir)
    if output is not None and output.exists() and output.stat().st_size > 0:
        return output, code
    return None, code


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rpt-014", type=Path, required=True)
    parser.add_argument("--rpt-015", type=Path, required=True)
    parser.add_argument("--rpt-bench", type=Path, required=True)
    parser.add_argument("--babeldoc", type=Path, required=True)
    parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--timeout", type=int, default=7200)
    parser.add_argument("--work", type=Path, default=Path("/tmp/rpt-prod-bench"))
    parser.add_argument("--pdf", action="append", type=Path, default=[])
    parser.add_argument(
        "--large",
        type=Path,
        default=None,
        help="existing ~45MB PDF; generated into the work directory when omitted",
    )
    parser.add_argument("--skip-large", action="store_true")
    parser.add_argument("--engines", default="rpt 0.1.4,rpt 0.1.5,babeldoc")
    args = parser.parse_args()
    if not os.environ.get("RPT_LLM_API_KEY"):
        print("RPT_LLM_API_KEY is required", file=sys.stderr)
        return 2
    if not Path("/usr/bin/time").exists():
        print("/usr/bin/time is required (apt install time)", file=sys.stderr)
        return 2
    for binary in (args.rpt_014, args.rpt_015, args.rpt_bench, args.babeldoc):
        if not binary.exists():
            print(f"missing binary {binary}", file=sys.stderr)
            return 2
    base_url = os.environ.get("RPT_LLM_BASE_URL", "https://hub.mypapers.top/api/llm/v1")
    model = os.environ.get("RPT_LLM_MODEL", "auto")
    api_key = os.environ["RPT_LLM_API_KEY"]
    engines = [item.strip() for item in args.engines.split(",") if item.strip()]
    work = args.work
    work.mkdir(parents=True, exist_ok=True)

    documents: list[tuple[str, Path]] = []
    papers = args.pdf or list(DEFAULT_PAPERS)
    for path in papers:
        if not path.exists():
            print(f"missing pdf {path}", file=sys.stderr)
            return 2
        documents.append((path.stem, path))
    if not args.skip_large:
        large = args.large
        if large is None:
            large = work / "large-30p.pdf"
            if not large.exists():
                print(f"generating {large}", flush=True)
                started = time.time()
                generate_large(large)
                print(f"generated in {time.time() - started:.1f}s bytes {large.stat().st_size}", flush=True)
        if not large.exists():
            print(f"missing large pdf {large}", file=sys.stderr)
            return 2
        documents.append(("large-30p", large))

    results_path = work / "results.jsonl"
    done = load_done(results_path)
    rows: list[dict] = []
    version = babeldoc_version(args.babeldoc)
    for name, source in documents:
        source_bytes = source.stat().st_size
        for engine in engines:
            key = (name, engine)
            if key in done and done[key].get("status") == "ok":
                print(f"== resume {engine} {name}", flush=True)
                rows.append(done[key])
                continue
            out_dir = work / name / engine.replace(" ", "-")
            print(f"== {engine} {name}", flush=True)
            started = time.time()
            pdf, code = translate(
                engine,
                source,
                out_dir,
                rpt_014=args.rpt_014,
                rpt_015=args.rpt_015,
                babeldoc=args.babeldoc,
                jobs=args.jobs,
                base_url=base_url,
                model=model,
                api_key=api_key,
                timeout=args.timeout,
            )
            elapsed_clock = time.time() - started
            time_text = (out_dir / "time.txt").read_text() if (out_dir / "time.txt").exists() else ""
            wall, rss = parse_time(time_text)
            if wall is None:
                wall = elapsed_clock
            own = coverage_from_stdout(out_dir / "stdout.txt") if engine.startswith("rpt") else None
            line_coverage = None
            status = "ok" if pdf is not None else "failed"
            detail = ""
            if pdf is None:
                err = (out_dir / "stderr.txt").read_text(errors="replace") if (out_dir / "stderr.txt").exists() else ""
                detail = scrub(err[-1500:])
                print(f"   failed exit {code}: {detail[-200:]}", flush=True)
            else:
                line_coverage = score_line_coverage(
                    args.rpt_bench, source, pdf, out_dir / "score.log"
                )
                print(
                    f"   wall {fmt_seconds(wall)} rss {fmt_mb(rss)} "
                    f"pdf {pdf.stat().st_size} coverage {line_coverage} exit {code}",
                    flush=True,
                )
            row = {
                "pdf": name,
                "pages": 30 if name == "large-30p" else None,
                "source": str(source),
                "source_bytes": source_bytes,
                "source_sha256": sha256(source),
                "engine": engine,
                "status": status,
                "exit_code": code,
                "wall_seconds": wall,
                "rss_kbytes": rss,
                "output_bytes": pdf.stat().st_size if pdf else None,
                "output": str(pdf) if pdf else None,
                "line_coverage": line_coverage,
                "rpt_coverage": own,
                "tool_coverage": tool_coverage(own) if engine.startswith("rpt") else "n/a",
                "detail": detail,
            }
            rows.append(row)
            with results_path.open("a") as handle:
                handle.write(json.dumps(row) + "\n")

    # Page counts for the CI papers are fixed in the manifest. Fill them for the table.
    manifest_pages = {
        "arxiv-2610.02163": 11,
        "arxiv-2610.03665": 16,
        "large-30p": 30,
    }
    for row in rows:
        if row.get("pages") is None:
            row["pages"] = manifest_pages.get(row["pdf"], "")
    meta = {
        "recorded_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "git": git_head(),
        "base_url": base_url,
        "model": model,
        "jobs": args.jobs,
        "babeldoc_version": version,
        "rpt_014": str(args.rpt_014),
        "rpt_015": str(args.rpt_015),
    }
    text = markdown(rows, meta)
    (work / "table.md").write_text(text)
    (work / "meta.json").write_text(json.dumps(meta, indent=2) + "\n")
    print(text)
    return 0 if any(row.get("status") == "ok" for row in rows) else 1


if __name__ == "__main__":
    raise SystemExit(main())
