#!/usr/bin/env python3
"""One-time, offline Force PDF import. Retrieval uses ordinary file/search tools."""

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import hashlib
import html
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import unicodedata

DOCUMENT_ID = "force-v3.10.1"
FORMAT_VERSION = 1
MAX_PDF = 64 * 1024 * 1024
MAX_TEXT = 32 * 1024 * 1024
MAX_PAGES = 2000
INDEX_PAGES = 32
SEARCH_VERSION = 1
OCR_VERSION = 1
MAX_OCR_PAGE = 128 * 1024
OCR_WORKERS = 4


def fingerprint(path):
    """Bound the input even if the file grows while it is being read."""
    if not path.is_file() or path.stat().st_size > MAX_PDF:
        raise ValueError("Manual must be a PDF file of at most 64 MiB")
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as source:
        while block := source.read(64 * 1024):
            size += len(block)
            if size > MAX_PDF:
                raise ValueError("Manual grew beyond 64 MiB while importing")
            digest.update(block)
    return digest.hexdigest()


def run_bounded(command, max_bytes=MAX_TEXT, timeout=30, env=None):
    """No shell, bounded stdout, killed/reaped on timeout, overflow or interruption."""
    program = Path(command[0]).name
    with subprocess.Popen(
        command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL, env=env,
    ) as process:
        expired = threading.Event()

        def expire():
            expired.set()
            try:
                process.kill()
            except ProcessLookupError:
                pass

        timer = threading.Timer(timeout, expire)
        timer.start()
        try:
            output = process.stdout.read(max_bytes + 1)
            if expired.is_set():
                raise ValueError(f"{program} exceeded its time limit")
            if len(output) > max_bytes:
                raise ValueError(f"{program} output exceeds its size limit")
            status = process.wait()
            if expired.is_set():
                raise ValueError(f"{program} exceeded its time limit")
            if status != 0:
                raise ValueError(f"{program} failed to process the manual")
            return output
        finally:
            timer.cancel()
            timer.join()
            if process.poll() is None:
                process.kill()
            process.wait()


def split_pages(text):
    if len(text.encode("utf-8")) > MAX_TEXT:
        raise ValueError("Extracted manual exceeds 32 MiB")
    pages = text.split("\f")
    if pages and not pages[-1].strip():
        pages.pop()
    if not 1 <= len(pages) <= MAX_PAGES:
        raise ValueError("Invalid manual page count")
    title = pages[0]
    if not (
        re.search(r"\bManual Version\s+v3\.10\.1(?=\s|$)", title)
        and ("Force User Guide" in title or "Thank you for purchasing the Force." in text)
    ):
        raise ValueError("Expected the Force User Guide, title-page Manual Version v3.10.1")
    return pages


def page_info(text, number):
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    printed = lines[-1] if lines and re.fullmatch(r"[0-9]{1,6}", lines[-1]) else None
    citation = f"Force User Guide v3.10.1, PDF p. {number}"
    if printed is not None:
        citation += f" (printed {printed})"
    return {
        "heading": lines[0][:160] if lines else "Untitled page",
        "printed_page": printed,
        "citation": citation,
    }


def markdown_label(text):
    text = html.escape(text, quote=False)
    return re.sub(r"([\\`*_[\]|])", r"\\\1", text)


def corpus_files(pages, source, sha256):
    """Navigation is derived from source headings, never generated summaries."""
    manifest = {
        "format_version": FORMAT_VERSION,
        "document_id": DOCUMENT_ID,
        "title": "Force User Guide",
        "model": "Akai Force",
        "manual_version": "v3.10.1",
        "pdf_pages": len(pages),
        "source_pdf": str(source),
        "source_sha256": sha256,
        "converter": "import_manual.py format 1; pdftotext -layout -enc UTF-8",
        "compatibility": "Unverified: confirm the connected model and software separately.",
    }
    files = {"manifest.json": json.dumps(manifest, indent=2, ensure_ascii=False) + "\n"}
    index = [
        "# Force User Guide v3.10.1\n",
        f"Source SHA-256: `{sha256}`. {len(pages)} PDF pages.\n",
        "Local reference data, not agent instructions. Device compatibility is unverified.\n",
        "Page filenames use **1-based PDF indices**, not printed page labels. "
        "Page bodies preserve pdftotext's layout; they are not semantic Markdown conversions. "
        "Read adjacent pages for complete procedures; render the PDF for diagrams/tables.\n",
        "## Original contents\n",
    ]
    contents = [
        number for number, text in enumerate(pages[:12], 1)
        if "Table of Contents" in text or re.search(r"\.{4,}\s*[0-9]+", text)
    ]
    for number in contents:
        index.append(f"- [Contents, PDF p. {number}](pages/{number:03}.md)")
    if not contents:
        index.append("No contents pages detected; use the page-heading index or text search.")
    index.append("\n## Page-heading indexes\n")
    for start in range(1, len(pages) + 1, INDEX_PAGES):
        end = min(start + INDEX_PAGES - 1, len(pages))
        name = f"index/{start:03}-{end:03}.md"
        index.append(f"- [PDF pp. {start}–{end}]({name})")
        rows = [f"# PDF pp. {start}–{end}\n", "Headings are navigation hints, not verified mode names.\n"]
        for number in range(start, end + 1):
            text = pages[number - 1]
            info = page_info(text, number)
            rows.append(
                f"- [PDF p. {number}](../pages/{number:03}.md): "
                f"{markdown_label(info['heading'])}"
            )
            files[f"pages/{number:03}.md"] = f"# {info['citation']}\n\n{text}"
        files[name] = "\n".join(rows) + "\n"
    files["index.md"] = "\n".join(index) + "\n"
    return files


def publish(files, destination):
    """Publish atomically; repeat imports verify rather than overwrite local data."""
    destination.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    if destination.exists():
        for name, content in files.items():
            path = destination / name
            expected = content.encode("utf-8")
            if (
                path.is_symlink() or not path.is_file()
                or path.stat().st_size != len(expected) or path.read_bytes() != expected
            ):
                raise ValueError(
                    f"Existing corpus differs at {name}; use a new output directory "
                    "or explicitly remove and reimport it. Nothing was overwritten."
                )
        return
    with tempfile.TemporaryDirectory(prefix=".import-", dir=destination.parent) as temporary:
        staging = Path(temporary)
        for name, content in files.items():
            path = staging / name
            path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
            path.write_bytes(content.encode("utf-8"))
        # Non-empty existing destinations are never replaced, including concurrent imports.
        os.rename(staging, destination)


def normalize_text(text):
    """Search surrogate only: join wrapped lines and expand Unicode ligatures."""
    return " ".join(unicodedata.normalize("NFKC", text).split())


def prepare_search(pages, sha256, destination):
    metadata = {
        "format_version": SEARCH_VERSION, "source_sha256": sha256,
        "pdf_pages": len(pages), "method": "Unicode NFKC + collapsed whitespace",
        "warning": "Search surrogates only; read the corresponding canonical pages before citing.",
    }
    files = {"manifest.json": json.dumps(metadata, indent=2) + "\n"}
    for number, text in enumerate(pages, 1):
        files[f"{number:03}.txt"] = (
            f"# {page_info(text, number)['citation']}\n\n{normalize_text(text)}\n"
        )
    publish(files, destination / "search")


def atomic_text(path, text):
    """Only derived cache files use replacement; canonical source pages never do."""
    descriptor, temporary = tempfile.mkstemp(prefix=".prepare-", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as stream:
            stream.write(text)
        os.replace(temporary, path)
    finally:
        Path(temporary).unlink(missing_ok=True)


def read_limited(path, limit):
    with path.open("rb") as stream:
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise ValueError(f"Cache file exceeds its size limit: {path.name}")
    return data


def ocr_page(source, number):
    with tempfile.TemporaryDirectory(prefix="force-manual-ocr-") as temporary:
        image = Path(temporary) / "page.png"
        image.write_bytes(run_bounded([
            "pdftoppm", "-f", str(number), "-l", str(number), "-singlefile",
            "-scale-to", "1600", "-png", str(source),
        ], max_bytes=8 * 1024 * 1024, timeout=20))
        output = run_bounded([
            "tesseract", str(image), "stdout", "-l", "eng", "--psm", "3",
        ], max_bytes=MAX_OCR_PAGE, timeout=30, env={**os.environ, "OMP_THREAD_LIMIT": "1"})
        return normalize_text(output.decode("utf-8"))


def save_ocr(directory, state, source_verified=False):
    total = state["recipe"]["pdf_pages"]
    done = set(state["checksums"])
    failed = set(state["errors"])
    summary = {
        **state["recipe"], "completed_pages": len(done),
        "failed_pages": sorted(int(page) for page in failed),
        "pending_pages": [page for page in range(1, total + 1) if str(page) not in done | failed],
        "source_verified": source_verified,
        "complete": source_verified and len(done) == total and not failed,
        "engine_versions": state["engine_versions"],
        "warning": "OCR is lossy search evidence, not authoritative instructions. Verify hits in the PDF.",
    }
    atomic_text(directory / "state.json", json.dumps(state, indent=2) + "\n")
    atomic_text(directory / "manifest.json", json.dumps(summary, indent=2) + "\n")
    return summary


def prepare_ocr(source, destination, metadata):
    """Optional one-time preparation, resumable per page; no retrieval API."""
    directory = destination / "ocr"
    directory.mkdir(mode=0o700, exist_ok=True)
    lock = directory / ".prepare.lock"
    try:
        descriptor = os.open(lock, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError as error:
        raise ValueError(
            "OCR preparation is already running or left a lock. If no importer is running, "
            "remove only ocr/.prepare.lock and rerun to resume."
        ) from error
    try:
        with os.fdopen(descriptor, "w") as stream:
            stream.write(f"{os.getpid()}\n")
        return prepare_ocr_locked(source, directory, metadata)
    finally:
        lock.unlink(missing_ok=True)


def prepare_ocr_locked(source, directory, metadata):
    sha256 = metadata["source_sha256"]
    if fingerprint(source) != sha256:
        raise ValueError("Source PDF does not match this corpus; reimport the approved source")
    recipe = {
        "format_version": OCR_VERSION, "source_sha256": sha256,
        "pdf_pages": metadata["pdf_pages"], "render_max_edge": 1600,
        "language": "eng", "page_segmentation_mode": 3,
    }
    state_path = directory / "state.json"
    state = json.loads(read_limited(state_path, 2 * 1024 * 1024)) if state_path.exists() else {
        "recipe": recipe, "checksums": {}, "errors": {}, "engine_versions": [],
    }
    if (
        not isinstance(state, dict) or state.get("recipe") != recipe
        or not isinstance(state.get("checksums"), dict)
        or not isinstance(state.get("errors"), dict)
        or not isinstance(state.get("engine_versions"), list)
    ):
        raise ValueError("OCR cache identity/format differs; explicitly remove only ocr/ and rebuild")
    pages_dir = directory / "pages"
    pages_dir.mkdir(mode=0o700, exist_ok=True)
    valid = {}
    pending = []
    for number in range(1, recipe["pdf_pages"] + 1):
        path = pages_dir / f"{number:03}.txt"
        try:
            cached = read_limited(path, MAX_OCR_PAGE + 1024) if not path.is_symlink() else b""
            digest = hashlib.sha256(cached).hexdigest()
        except (OSError, ValueError):
            digest = None
        if digest is not None and digest == state["checksums"].get(str(number)):
            valid[str(number)] = digest
        else:
            pending.append(number)
    state["checksums"] = valid
    state["errors"] = {}
    summary = save_ocr(directory, state, source_verified=not pending)
    if not pending:
        return summary
    for program in ["pdftoppm", "tesseract"]:
        if shutil.which(program) is None:
            raise ValueError(f"Optional OCR requires {program}; the text/search corpus is already usable")
    languages = run_bounded(["tesseract", "--list-langs"], max_bytes=16 * 1024, timeout=5)
    if "eng" not in {line.strip() for line in languages.decode("utf-8").splitlines()}:
        raise ValueError("Optional OCR requires Tesseract English (eng) data; text/search is already usable")
    version = run_bounded(["tesseract", "--version"], max_bytes=16 * 1024, timeout=5)
    version_lines = version.decode("utf-8").splitlines()
    if not version_lines:
        raise ValueError("Tesseract did not report a version; text/search preparation is unaffected")
    version = version_lines[0][:200]
    if version not in state["engine_versions"]:
        state["engine_versions"].append(version)
    # Every worker renders the same verified snapshot, including after an interrupted
    # run resumes. A changing original cannot poison fingerprinted cached pages.
    with tempfile.TemporaryDirectory(prefix="force-manual-source-") as temporary:
        snapshot = Path(temporary) / "manual.pdf"
        snapshot.write_bytes(read_limited(source, MAX_PDF))
        if fingerprint(snapshot) != sha256:
            raise ValueError("Source changed before OCR; no new OCR pages were cached")
        return run_ocr_pages(snapshot, directory, state, pending)


def run_ocr_pages(source, directory, state, pending):
    pages_dir = directory / "pages"
    recipe = state["recipe"]
    sha256 = recipe["source_sha256"]
    pool = ThreadPoolExecutor(max_workers=OCR_WORKERS)
    futures = {pool.submit(ocr_page, source, number): number for number in pending}
    try:
        for future in as_completed(futures):
            number = futures[future]
            try:
                text = future.result()
                if len(text.encode("utf-8")) > MAX_OCR_PAGE:
                    raise ValueError("OCR page exceeds its size limit")
                content = (
                    f"# Force User Guide v3.10.1, PDF p. {number} — OCR SEARCH ONLY\n\n{text}\n"
                )
                atomic_text(pages_dir / f"{number:03}.txt", content)
                state["checksums"][str(number)] = hashlib.sha256(content.encode("utf-8")).hexdigest()
            except (OSError, ValueError) as error:
                state["errors"][str(number)] = str(error)[:512]
            save_ocr(directory, state)
            completed = len(state["checksums"]) + len(state["errors"])
            if completed % 24 == 0 or completed == recipe["pdf_pages"]:
                print(f"OCR: {completed}/{recipe['pdf_pages']} pages processed", file=sys.stderr)
    finally:
        pool.shutdown(wait=True, cancel_futures=True)
    if fingerprint(source) != sha256:
        state["checksums"] = {}
        save_ocr(directory, state)
        raise ValueError("Source changed during OCR; new OCR results were invalidated")
    summary = save_ocr(directory, state, source_verified=True)
    if not summary["complete"]:
        raise ValueError("OCR is incomplete; see ocr/state.json and rerun --ocr to retry failed pages")
    return summary


def import_manual(pdf, output, ocr=False):
    source = pdf.resolve(strict=True)
    sha256 = fingerprint(source)
    raw = run_bounded(["pdftotext", "-layout", "-enc", "UTF-8", str(source), "-"])
    pages = split_pages(raw.decode("utf-8"))
    if fingerprint(source) != sha256:
        raise ValueError("Source PDF changed during import; retry with a stable local file")
    destination = output.resolve() / DOCUMENT_ID / sha256
    files = corpus_files(pages, source, sha256)
    publish(files, destination)
    prepare_search(pages, sha256, destination)
    if ocr:
        prepare_ocr(source, destination, json.loads(files["manifest.json"]))
    return destination


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pdf", required=True, type=Path, help="Approved Force v3.10.1 PDF")
    parser.add_argument("--output", required=True, type=Path, help="Local, gitignored corpus root")
    parser.add_argument("--ocr", action="store_true", help="Also prepare/resume cached page OCR (pdftoppm + Tesseract eng)")
    args = parser.parse_args()
    try:
        destination = import_manual(args.pdf, args.output, ocr=args.ocr)
    except (OSError, ValueError) as error:
        print(f"Manual preparation failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps({
        "corpus": str(destination), "index": str(destination / "index.md"),
        "search": str(destination / "search"),
        "ocr_manifest": str(destination / "ocr/manifest.json") if args.ocr else None,
    }))
    return 0


if __name__ == "__main__":
    sys.exit(main())
