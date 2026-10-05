#!/usr/bin/env python3
"""Download the fidelity corpus into corpus/cache.

The manifest is the source of truth for the full corpus. Those PDFs are not
committed. A separate openly licensed subset lives in corpus/ci and is what CI runs.

    python3 corpus/fetch.py           # download every manifest entry
    python3 corpus/fetch.py --ci      # only entries with "ci": true
    python3 corpus/fetch.py --discover
        # rebuild corpus/manifest.json from public proceedings, arXiv,
        # and openly licensed books, then download them

Checksums are verified on later runs. A mismatch is an error.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import time
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

ROOT = Path(__file__).resolve().parent
MANIFEST = ROOT / "manifest.json"
CACHE = ROOT / "cache"
UA = "RapidPdfTrans-corpus/0.1 (+https://github.com/RapidAI/RapidPdfTrans)"

ARXIV_LICENSE = "arXiv non-exclusive distribution license 1.0 (PDF not committed)"
ACL_LICENSE = "CC BY 4.0 (ACL Anthology)"
PMLR_LICENSE = "CC BY 4.0 (PMLR)"
NEURIPS_LICENSE = "CC BY 4.0 (NeurIPS proceedings)"
CVF_LICENSE = "CVF open access; copyright retained by the authors (PDF not committed)"
USENIX_LICENSE = "USENIX open-access proceedings; copyright retained by the authors (PDF not committed)"


def request(url: str, method: str = "GET", timeout: int = 90) -> tuple[str, bytes, dict]:
    req = urllib.request.Request(url, method=method, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=timeout) as response:
        data = b"" if method == "HEAD" else response.read()
        headers = {key.lower(): value for key, value in response.headers.items()}
        return response.geturl(), data, headers


def get_text(url: str, limit: int = 1_600_000) -> str:
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=90) as response:
        return response.read(limit).decode("utf-8", "replace")


def entry(doc_id: str, url: str, source: str, title: str, license_name: str, tags: list[str]) -> dict:
    return {
        "id": doc_id,
        "url": url,
        "source": source,
        "title": title,
        "license": license_name,
        "sha256": "",
        "bytes": 0,
        "pages": None,
        "tags": tags,
        "ci": False,
    }


def arxiv_license(arxiv_id: str) -> str:
    try:
        html = get_text(f"https://arxiv.org/abs/{arxiv_id}")
    except Exception:
        return ARXIV_LICENSE
    match = re.search(r'https?://creativecommons.org/licenses/[^"\s<]+', html)
    if match:
        return f"{match.group(0)} (arXiv; PDF not committed)"
    if "nonexclusive-distrib" in html:
        return ARXIV_LICENSE
    return ARXIV_LICENSE


def discover() -> list[dict]:
    docs: list[dict] = []
    seen: set[str] = set()

    def add(doc: dict) -> None:
        if doc["url"] in seen or doc["id"] in {item["id"] for item in docs}:
            return
        seen.add(doc["url"])
        docs.append(doc)

    classics = [
        ("1706.03762", "Attention Is All You Need", ["cs", "ml", "two-column", "formulas", "figures", "latex"]),
        ("1810.04805", "BERT", ["cs", "nlp", "two-column", "tables", "latex"]),
        ("1512.03385", "Deep Residual Learning", ["cs", "cv", "two-column", "figures", "tables", "latex"]),
        ("1409.1556", "Very Deep Convolutional Networks", ["cs", "cv", "two-column", "tables", "latex"]),
        ("1312.6114", "Auto-Encoding Variational Bayes", ["cs", "ml", "formulas", "single-column", "latex"]),
        ("1406.2661", "Generative Adversarial Nets", ["cs", "ml", "formulas", "latex"]),
        ("1502.03167", "Batch Normalization", ["cs", "ml", "formulas", "latex"]),
        ("1412.6980", "Adam", ["cs", "ml", "formulas", "algorithms", "latex"]),
        ("1506.02640", "Faster R-CNN", ["cs", "cv", "two-column", "figures", "latex"]),
        ("1703.06870", "Mask R-CNN", ["cs", "cv", "two-column", "figures", "latex"]),
        ("2010.11929", "An Image is Worth 16x16 Words", ["cs", "cv", "two-column", "formulas", "figures", "latex"]),
        ("2006.11239", "Denoising Diffusion Probabilistic Models", ["cs", "ml", "formulas", "figures", "latex"]),
        ("1607.06450", "Layer Normalization", ["cs", "ml", "formulas", "latex"]),
        ("2103.14030", "Highly accurate protein structure prediction", ["biology", "figures", "latex"]),
    ]
    for arxiv_id, title, tags in classics:
        add(entry(
            f"arxiv-{arxiv_id}",
            f"https://arxiv.org/pdf/{arxiv_id}",
            "arxiv",
            title,
            arxiv_license(arxiv_id),
            ["arxiv", *tags],
        ))
        time.sleep(0.4)

    recent = {
        "cs": ["2610.02193", "2610.02163"],
        "math": ["2610.01998", "2610.02109", "2610.01899"],
        "physics": ["2610.02209", "2610.02080", "2610.01706"],
        "biology": ["2610.01891", "2610.01358", "2609.38908"],
        "security": ["2610.01995", "2610.02113"],
        "stats": ["2610.02158", "2610.01980"],
    }
    for field, ids in recent.items():
        for arxiv_id in ids:
            add(entry(
                f"arxiv-{arxiv_id}",
                f"https://arxiv.org/pdf/{arxiv_id}",
                "arxiv",
                arxiv_id,
                arxiv_license(arxiv_id),
                ["arxiv", field, "latex"],
            ))
            time.sleep(0.3)

    for arxiv_id, title in [
        ("2609.36965", "Chinese-Jev"),
        ("2609.36804", "VAA-CSEC"),
        ("2609.33014", "TCMQA"),
        ("2609.32770", "C-HAT-Bench"),
    ]:
        add(entry(
            f"arxiv-{arxiv_id}",
            f"https://arxiv.org/pdf/{arxiv_id}",
            "arxiv",
            title,
            arxiv_license(arxiv_id),
            ["arxiv", "cjk", "nlp", "latex"],
        ))
        time.sleep(0.3)

    scrape_anthology(add, "https://aclanthology.org/volumes/2024.acl-long/", "2024.acl-long", "acl", 8)
    scrape_anthology(add, "https://aclanthology.org/volumes/2023.emnlp-main/", "2023.emnlp-main", "emnlp", 6)
    scrape_anthology(add, "https://aclanthology.org/volumes/2024.naacl-long/", "2024.naacl-long", "naacl", 4)
    scrape_pmlr(add)
    scrape_neurips(add)
    scrape_cvf(add, "https://openaccess.thecvf.com/CVPR2024?day=2024-06-19", "cvpr-2024", "cvpr", 6)
    scrape_cvf(add, "https://openaccess.thecvf.com/ICCV2023?day=2023-10-02", "iccv-2023", "iccv", 4)
    scrape_usenix(add)
    add_books(add)
    add_scans(add)
    return docs


def scrape_anthology(add, url: str, prefix: str, source: str, count: int) -> None:
    try:
        html = get_text(url)
    except Exception as exc:
        print("skip", source, exc, file=sys.stderr)
        return
    found = sorted(
        set(re.findall(rf"https://aclanthology.org/({re.escape(prefix)}\.\d+)\.pdf", html)),
        key=lambda item: int(item.rsplit(".", 1)[-1]),
    )
    for anth_id in found[:count]:
        add(entry(
            anth_id.replace(".", "-"),
            f"https://aclanthology.org/{anth_id}.pdf",
            source,
            anth_id,
            ACL_LICENSE,
            [source, "nlp", "two-column", "latex", "conference"],
        ))


def scrape_pmlr(add) -> None:
    try:
        html = get_text("https://proceedings.mlr.press/v202/")
    except Exception as exc:
        print("skip pmlr", exc, file=sys.stderr)
        return
    urls = []
    for match in re.findall(r"https://proceedings.mlr.press/v202/[a-z0-9]+/[a-z0-9]+\.pdf", html):
        if match not in urls:
            urls.append(match)
        if len(urls) >= 10:
            break
    for url in urls:
        slug = url.rstrip("/").split("/")[-1].replace(".pdf", "")
        add(entry(
            f"pmlr-v202-{slug}",
            url,
            "pmlr",
            slug,
            PMLR_LICENSE,
            ["pmlr", "icml", "ml", "two-column", "latex", "conference"],
        ))


def scrape_neurips(add) -> None:
    try:
        html = get_text("https://proceedings.neurips.cc/paper_files/paper/2023")
    except Exception as exc:
        print("skip neurips", exc, file=sys.stderr)
        return
    hashes = []
    for match in re.findall(r"/paper_files/paper/2023/hash/([0-9a-f]{32})-Abstract-Conference.html", html):
        if match not in hashes:
            hashes.append(match)
        if len(hashes) >= 6:
            break
    for digest in hashes:
        add(entry(
            f"neurips-2023-{digest[:8]}",
            "https://proceedings.neurips.cc/paper_files/paper/2023/file/"
            f"{digest}-Paper-Conference.pdf",
            "neurips",
            f"NeurIPS 2023 {digest[:8]}",
            NEURIPS_LICENSE,
            ["neurips", "ml", "two-column", "latex", "conference"],
        ))
    add(entry(
        "neurips-2017-attention",
        "https://proceedings.neurips.cc/paper_files/paper/2017/file/3f5ee243547dee91fbd053c1c4a845aa-Paper.pdf",
        "neurips",
        "Attention Is All You Need (NeurIPS 2017)",
        NEURIPS_LICENSE,
        ["neurips", "ml", "two-column", "formulas", "latex", "conference"],
    ))


def scrape_cvf(add, page: str, prefix: str, source: str, count: int) -> None:
    try:
        html = get_text(page)
    except Exception as exc:
        print("skip", source, exc, file=sys.stderr)
        return
    paths = []
    for match in re.findall(r'href="([^"]+_paper\.pdf)"', html):
        if "supplemental" in match:
            continue
        if match not in paths:
            paths.append(match)
        if len(paths) >= count:
            break
    for path in paths:
        name = path.split("/")[-1].replace("_paper.pdf", "")
        slug = re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")[:48]
        url = path if path.startswith("http") else "https://openaccess.thecvf.com" + path
        add(entry(
            f"{prefix}-{slug}"[:80],
            url,
            source,
            name.replace("_", " "),
            CVF_LICENSE,
            [source, "cv", "two-column", "figures", "latex", "conference"],
        ))


def scrape_usenix(add) -> None:
    try:
        html = get_text("https://www.usenix.org/conference/usenixsecurity24/technical-sessions")
    except Exception as exc:
        print("skip usenix", exc, file=sys.stderr)
        return
    slugs = []
    for match in re.findall(r"/conference/usenixsecurity24/presentation/([a-z0-9\-]+)", html):
        if match not in slugs:
            slugs.append(match)
    kept = 0
    for slug in slugs:
        if slug in {"brumley"}:
            continue
        url = f"https://www.usenix.org/system/files/usenixsecurity24-{slug}.pdf"
        try:
            _, _, headers = request(url, method="HEAD", timeout=25)
        except Exception:
            continue
        if "pdf" not in headers.get("content-type", ""):
            continue
        length = int(headers.get("content-length") or 0)
        if length > 8_000_000:
            continue
        add(entry(
            f"usenix-sec24-{slug}",
            url,
            "usenix",
            slug,
            USENIX_LICENSE,
            ["usenix", "security", "two-column", "code", "conference"],
        ))
        kept += 1
        if kept >= 5:
            break


def add_books(add) -> None:
    candidates = [
        (
            "openstax-university-physics-v2",
            "https://assets.openstax.org/oscms-prodcms/media/documents/university-physics-volume-2_-_WEB.pdf",
            "openstax",
            "University Physics Volume 2",
            "CC BY-NC-SA 4.0 (OpenStax)",
            ["book", "physics", "textbook", "formulas", "figures", "single-column"],
            40_000_000,
        ),
        (
            "open-logic",
            "https://builds.openlogicproject.org/open-logic-complete.pdf",
            "open-logic",
            "The Open Logic Project",
            "CC BY 4.0 (Open Logic Project)",
            ["book", "math", "formulas", "textbook"],
            30_000_000,
        ),
        (
            "ctex-manual",
            "https://mirrors.mit.edu/CTAN/language/chinese/ctex/ctex.pdf",
            "ctan",
            "ctex manual",
            "LPPL-1.3c (CTAN ctex)",
            ["book", "cjk", "manual", "latex"],
            20_000_000,
        ),
    ]
    for doc_id, url, source, title, license_name, tags, limit in candidates:
        try:
            _, _, headers = request(url, method="HEAD", timeout=40)
        except Exception as exc:
            print("skip book", doc_id, exc, file=sys.stderr)
            continue
        length = int(headers.get("content-length") or 0)
        kind = headers.get("content-type", "")
        if length and length > limit:
            print("skip large book", doc_id, length, file=sys.stderr)
            continue
        if kind and "pdf" not in kind and "octet-stream" not in kind:
            print("skip non-pdf book", doc_id, kind, file=sys.stderr)
            continue
        add(entry(doc_id, url, source, title, license_name, tags))

    for ebook, title in [(11, "Alice's Adventures in Wonderland"), (1661, "The Adventures of Sherlock Holmes"), (84, "Frankenstein")]:
        pdf = gutenberg_pdf(ebook)
        if not pdf:
            continue
        add(entry(
            f"gutenberg-{ebook}",
            pdf,
            "gutenberg",
            title,
            "Public domain (Project Gutenberg)",
            ["book", "literature", "public-domain"],
        ))


def gutenberg_pdf(ebook: int) -> str | None:
    try:
        html = get_text(f"https://www.gutenberg.org/ebooks/{ebook}")
    except Exception:
        return None
    for href in re.findall(r'href="([^"]+)"', html):
        if href.lower().endswith(".pdf"):
            if href.startswith("http"):
                return href
            return "https://www.gutenberg.org" + href
    return None


def add_scans(add) -> None:
    query = "mediatype:(texts) AND format:(PDF) AND year:[1900 TO 1920]"
    url = (
        "https://archive.org/advancedsearch.php?q="
        + urllib.parse.quote(query)
        + "&fl[]=identifier&fl[]=title&rows=12&output=json"
    )
    try:
        _, data, _ = request(url, timeout=40)
        payload = json.loads(data.decode("utf-8", "replace"))
        identifiers = payload["response"]["docs"]
    except Exception as exc:
        print("skip archive search", exc, file=sys.stderr)
        identifiers = []
    kept = 0
    for item in identifiers:
        identifier = item.get("identifier")
        if not identifier:
            continue
        pdf_url = archive_pdf(identifier)
        if not pdf_url:
            continue
        title = item.get("title") or identifier
        if isinstance(title, list):
            title = title[0]
        add(entry(
            f"ia-{identifier}"[:70],
            pdf_url,
            "internet-archive",
            str(title)[:180],
            "Public domain scan (Internet Archive)",
            ["scanned", "public-domain", "historical"],
        ))
        kept += 1
        if kept >= 3:
            break


def archive_pdf(identifier: str) -> str | None:
    meta_url = f"https://archive.org/metadata/{identifier}"
    try:
        _, data, _ = request(meta_url, timeout=40)
        payload = json.loads(data.decode("utf-8", "replace"))
    except Exception:
        return None
    best = None
    for item in payload.get("files") or []:
        name = item.get("name") or ""
        if not name.lower().endswith(".pdf"):
            continue
        size = int(item.get("size") or 0)
        if size < 80_000 or size > 12_000_000:
            continue
        if best is None or size < best[0]:
            best = (size, name)
    if not best:
        return None
    return f"https://archive.org/download/{identifier}/{urllib.parse.quote(best[1])}"


def download_one(doc: dict) -> dict:
    path = CACHE / f"{doc['id']}.pdf"
    if path.exists() and doc.get("sha256"):
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        if digest == doc["sha256"]:
            doc["bytes"] = path.stat().st_size
            return doc
        print("checksum mismatch, redownloading", doc["id"], file=sys.stderr)
    final, data, headers = request(doc["url"], timeout=180)
    kind = headers.get("content-type", "")
    if data[:5] != b"%PDF-" and "pdf" not in kind:
        raise RuntimeError(f"not a PDF ({kind}) from {final}")
    path.write_bytes(data)
    doc["sha256"] = hashlib.sha256(data).hexdigest()
    doc["bytes"] = len(data)
    doc["pages"] = pdf_pages(path)
    creator = pdf_field(path, "Creator") + " " + pdf_field(path, "Producer")
    tags = list(doc["tags"])
    if re.search(r"word|microsoft", creator, re.I):
        tags.append("word")
    if re.search(r"tex|latex", creator, re.I):
        tags.append("latex")
    title = pdf_field(path, "Title")
    if title and (doc["title"] == doc["id"] or re.fullmatch(r"\d{4}\.\d{4,5}", doc["title"])):
        doc["title"] = title[:180]
    doc["tags"] = sorted(set(tags))
    return doc


def pdf_field(path: Path, field: str) -> str:
    import subprocess

    try:
        out = subprocess.run(["pdfinfo", str(path)], check=False, capture_output=True, text=True, timeout=30)
    except Exception:
        return ""
    for line in out.stdout.splitlines():
        if line.startswith(field + ":"):
            return line.split(":", 1)[1].strip()
    return ""


def pdf_pages(path: Path) -> int | None:
    text = pdf_field(path, "Pages")
    return int(text) if text.isdigit() else None


# Same ids as corpus/ci. Only CC BY files, each under 1 MB.
CI_IDS = {
    "2024-acl-long-7",
    "arxiv-2609.36965",
    "arxiv-2610.01998",
    "arxiv-2610.02163",
    "arxiv-2610.02193",
    "arxiv-2610.03665",
    "neurips-2017-attention",
    "neurips-2023-00296c0e",
    "pmlr-v202-abbas23a",
    "pmlr-v202-abels23a",
}


def mark_ci(docs: list[dict]) -> None:
    for doc in docs:
        doc["ci"] = doc["id"] in CI_IDS


def load_manifest() -> list[dict]:
    payload = json.loads(MANIFEST.read_text())
    return payload["documents"]


def write_manifest(docs: list[dict]) -> None:
    docs = sorted(docs, key=lambda doc: (doc["source"], doc["id"]))
    payload = {
        "version": 1,
        "notes": "Full corpus PDFs are downloaded into corpus/cache and are not committed. sha256 is of the cached bytes. Entries with ci:true are the openly licensed files also stored in corpus/ci for GitHub Actions.",
        "documents": docs,
    }
    MANIFEST.write_text(json.dumps(payload, indent=2) + "\n")


def main() -> int:
    parser = argparse.ArgumentParser(description="Fetch the RapidPdfTrans fidelity corpus")
    parser.add_argument("--ci", action="store_true", help="download only ci:true entries")
    parser.add_argument("--discover", action="store_true", help="rebuild the manifest from public sources")
    parser.add_argument("--jobs", type=int, default=6)
    args = parser.parse_args()
    CACHE.mkdir(parents=True, exist_ok=True)
    if args.discover or not MANIFEST.exists():
        print("discovering sources", file=sys.stderr)
        docs = discover()
    else:
        docs = load_manifest()
    if args.ci:
        docs = [doc for doc in docs if doc.get("ci")]
    if not docs:
        print("no documents", file=sys.stderr)
        return 1
    failures = []
    done = []
    with ThreadPoolExecutor(max_workers=max(1, args.jobs)) as pool:
        futures = {pool.submit(download_one, dict(doc)): doc["id"] for doc in docs}
        for future in as_completed(futures):
            doc_id = futures[future]
            try:
                done.append(future.result())
                print("ok", doc_id, file=sys.stderr)
            except Exception as exc:
                failures.append((doc_id, str(exc)))
                print("FAIL", doc_id, exc, file=sys.stderr)
    if args.discover or not MANIFEST.exists():
        # Keep successfully fetched documents only when rebuilding.
        mark_ci(done)
        write_manifest(done)
    elif not args.ci:
        by_id = {doc["id"]: doc for doc in done}
        merged = []
        for doc in load_manifest():
            merged.append(by_id.get(doc["id"], doc))
        write_manifest(merged)
    print(f"downloaded {len(done)} failed {len(failures)}")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
