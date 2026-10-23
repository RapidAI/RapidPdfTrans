#!/usr/bin/env python3
"""Render source | rpt | BabelDOC strips for the README comparison.

One image per page, about 110 dpi. pngquant runs first. If the PNG is still
above ``--max-bytes``, the script writes a WebP instead. No network and no
API key.

    python3 corpus/bench/compare_pages.py \
        --source corpus/ci/arxiv-2610.02163.pdf \
        --rpt /tmp/rpt-02163.pdf \
        --babel /tmp/babel-02163.pdf \
        --pages 1,6,8 \
        --out-dir docs/compare \
        --prefix arxiv-2610.02163
"""

from __future__ import annotations

import argparse
import shutil
import subprocess
import tempfile
from pathlib import Path

import pymupdf


def pages_of(text: str) -> list[int]:
    pages: list[int] = []
    for part in text.split(","):
        part = part.strip()
        if not part:
            continue
        if "-" in part:
            start, end = part.split("-", 1)
            pages.extend(range(int(start), int(end) + 1))
        else:
            pages.append(int(part))
    if not pages:
        raise SystemExit("no pages")
    return pages


def render_page(doc: pymupdf.Document, page_no: int, dpi: float) -> pymupdf.Pixmap:
    if page_no < 1 or page_no > doc.page_count:
        raise SystemExit(f"page {page_no} is outside 1..{doc.page_count}")
    zoom = dpi / 72.0
    return doc[page_no - 1].get_pixmap(matrix=pymupdf.Matrix(zoom, zoom), alpha=False)


def stitch(pixmaps: list[pymupdf.Pixmap], labels: list[str], dpi: float) -> pymupdf.Pixmap:
    gap = 6
    header = 28
    width = sum(pix.width for pix in pixmaps) + gap * (len(pixmaps) - 1)
    height = header + max(pix.height for pix in pixmaps)
    scale = 72.0 / dpi
    doc = pymupdf.open()
    page = doc.new_page(width=width * scale, height=height * scale)
    page.draw_rect(page.rect, color=(1, 1, 1), fill=(1, 1, 1))
    x = 0
    for pix, label in zip(pixmaps, labels, strict=True):
        rect = pymupdf.Rect(x * scale, header * scale, (x + pix.width) * scale, (header + pix.height) * scale)
        page.insert_image(rect, pixmap=pix)
        page.insert_text(
            pymupdf.Point(x * scale + 4, 16 * scale),
            label,
            fontsize=11,
            fontname="helv",
            color=(0.1, 0.1, 0.1),
        )
        x += pix.width + gap
    out = page.get_pixmap(dpi=int(round(dpi)), alpha=False)
    doc.close()
    return out


def compress(png_path: Path, max_bytes: int) -> Path:
    pngquant = shutil.which("pngquant")
    if pngquant:
        for quality in ("60-90", "40-75", "20-55", "10-40"):
            dest = png_path.with_suffix(".quant.png")
            proc = subprocess.run(
                [pngquant, "--force", "--skip-if-larger", f"--quality={quality}", "--output", str(dest), str(png_path)],
                capture_output=True,
                text=True,
            )
            if proc.returncode == 0 and dest.exists() and dest.stat().st_size <= max_bytes:
                dest.replace(png_path)
                return png_path
            if dest.exists():
                if dest.stat().st_size < png_path.stat().st_size:
                    dest.replace(png_path)
                else:
                    dest.unlink()
            if proc.returncode not in (0, 98, 99):
                raise SystemExit(proc.stderr.strip() or f"pngquant exited {proc.returncode}")
    if png_path.stat().st_size <= max_bytes:
        return png_path
    cwebp = shutil.which("cwebp")
    if not cwebp:
        raise SystemExit(f"{png_path} is {png_path.stat().st_size} bytes and cwebp is not installed")
    webp = png_path.with_suffix(".webp")
    for quality in (75, 60, 45, 35, 25):
        proc = subprocess.run(
            [cwebp, "-quiet", "-q", str(quality), str(png_path), "-o", str(webp)],
            capture_output=True,
            text=True,
        )
        if proc.returncode != 0:
            raise SystemExit(proc.stderr.strip() or f"cwebp exited {proc.returncode}")
        if webp.stat().st_size <= max_bytes:
            png_path.unlink()
            return webp
    return webp


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--rpt", type=Path, required=True)
    parser.add_argument("--babel", type=Path, required=True)
    parser.add_argument("--pages", required=True, help="1-based pages, e.g. 1,6,8 or 2-4")
    parser.add_argument("--out-dir", type=Path, required=True)
    parser.add_argument("--prefix", required=True)
    parser.add_argument("--dpi", type=float, default=110)
    parser.add_argument("--max-bytes", type=int, default=300_000)
    parser.add_argument(
        "--labels",
        default="source,rpt 0.1.5,BabelDOC 0.6.4",
        help="Comma-separated column labels",
    )
    args = parser.parse_args()
    labels = [part.strip() for part in args.labels.split(",")]
    if len(labels) != 3:
        raise SystemExit("--labels needs three comma-separated names")
    args.out_dir.mkdir(parents=True, exist_ok=True)
    docs = [pymupdf.open(path) for path in (args.source, args.rpt, args.babel)]
    written: list[Path] = []
    with tempfile.TemporaryDirectory() as tmp:
        tmp_dir = Path(tmp)
        for page_no in pages_of(args.pages):
            pixmaps = [render_page(doc, page_no, args.dpi) for doc in docs]
            strip = stitch(pixmaps, labels, args.dpi)
            raw = tmp_dir / f"{args.prefix}-p{page_no:02d}.png"
            strip.save(raw)
            final = args.out_dir / raw.name
            final.write_bytes(raw.read_bytes())
            final = compress(final, args.max_bytes)
            size = final.stat().st_size
            print(f"{final} {strip.width}x{strip.height} {size}")
            if size > args.max_bytes:
                raise SystemExit(f"{final} is {size} bytes, above {args.max_bytes}")
            written.append(final)
    for doc in docs:
        doc.close()


if __name__ == "__main__":
    main()
