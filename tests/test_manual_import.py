"""Offline corpus tests. Synthetic references only, unless explicitly opted in."""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / ".agents/skills/force-teacher/scripts/import_manual.py"
spec = importlib.util.spec_from_file_location("manual_import", SCRIPT)
manual = importlib.util.module_from_spec(spec)
spec.loader.exec_module(manual)
TEXT = (
    "Force User Guide\nManual Version v3.10.1\n\f"
    "Sample Edit\nUse Chop Mode to slice a drum loop.\n99\n\f"
    "Mixer\nAdjust volume and pan.\n100\n\f"
)


def write_pdf(path):
    """Minimal genuine PDF containing only synthetic text; no copyrighted fixture."""
    pages = TEXT.rstrip("\f").split("\f")
    objects = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [4 0 R 6 0 R 8 0 R] /Count 3 >>",
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    ]
    for index, text in enumerate(pages):
        stream = "BT /F1 12 Tf 20 370 Td 18 TL " + " ".join(
            f"({line}) Tj T*" for line in text.splitlines()
        ) + " ET"
        objects.append(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] "
            f"/Resources << /Font << /F1 3 0 R >> >> /Contents {5 + index * 2} 0 R >>"
        )
        objects.append(f"<< /Length {len(stream)} >>\nstream\n{stream}\nendstream")
    pdf = b"%PDF-1.4\n"
    offsets = []
    for index, obj in enumerate(objects, 1):
        offsets.append(len(pdf))
        pdf += f"{index} 0 obj\n{obj}\nendobj\n".encode()
    xref = len(pdf)
    pdf += f"xref\n0 {len(objects) + 1}\n0000000000 65535 f \n".encode()
    pdf += "".join(f"{offset:010} 00000 n \n" for offset in offsets).encode()
    pdf += (
        f"trailer << /Size {len(objects) + 1} /Root 1 0 R >>\n"
        f"startxref\n{xref}\n%%EOF\n"
    ).encode()
    path.write_bytes(pdf)


class ManualImportTests(unittest.TestCase):
    def test_page_boundaries_citations_and_verbatim_bodies(self):
        pages = manual.split_pages(TEXT)
        files = manual.corpus_files(pages, Path("/synthetic.pdf"), "a" * 64)
        self.assertEqual(len(pages), 3)
        page = files["pages/002.md"]
        self.assertTrue(page.endswith(pages[1]))
        self.assertIn("PDF p. 2 (printed 99)", page)
        self.assertNotIn("Mixer", page)
        self.assertIn("Sample Edit", files["index/001-003.md"])
        manifest = json.loads(files["manifest.json"])
        self.assertEqual(manifest["manual_version"], "v3.10.1")
        self.assertEqual(manifest["source_sha256"], "a" * 64)
        self.assertIn("Unverified", manifest["compatibility"])

    def test_indexes_are_small_linked_and_escape_source_headings(self):
        pages = manual.split_pages(TEXT) + ["[not a link]|<script>\n"] * 65
        files = manual.corpus_files(pages, Path("/synthetic.pdf"), "b" * 64)
        self.assertEqual(len([name for name in files if name.startswith("index/")]), 3)
        self.assertIn("index/065-068.md", files["index.md"])
        self.assertIn("../pages/068.md", files["index/065-068.md"])
        self.assertIn(r"\[not a link\]\|&lt;script&gt;", files["index/065-068.md"])
        self.assertLess(len(files["index.md"]), 2048)

    def test_original_contents_links_not_guessed_printed_page_mapping(self):
        pages = manual.split_pages(TEXT)
        pages[1] = "Table of Contents\nChop Mode ........ 99\n"
        files = manual.corpus_files(pages, Path("/synthetic.pdf"), "c" * 64)
        self.assertIn("(pages/002.md)", files["index.md"])
        self.assertNotIn("pages/099.md", files["index.md"])

    def test_reject_wrong_version_model_empty_and_oversized_text(self):
        for text in [
            "", "\f", TEXT.replace("v3.10.1", "v3.0"),
            TEXT.replace("v3.10.1", "v3.10.10"), TEXT.replace("Force", "Other"),
        ]:
            with self.subTest(text=text), self.assertRaises(ValueError):
                manual.split_pages(text)
        with patch.object(manual, "MAX_TEXT", 8), self.assertRaises(ValueError):
            manual.split_pages(TEXT)
        with patch.object(manual, "MAX_PAGES", 2), self.assertRaises(ValueError):
            manual.split_pages(TEXT)

    def test_atomic_publish_idempotence_and_corruption_rejection(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / "corpus"
            files = manual.corpus_files(manual.split_pages(TEXT), Path("/synthetic.pdf"), "d" * 64)
            manual.publish(files, destination)
            manual.publish(files, destination)
            page = destination / "pages/002.md"
            page.write_text("corrupt", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "Nothing was overwritten"):
                manual.publish(files, destination)
            self.assertEqual(page.read_text(), "corrupt")
            self.assertEqual(list(Path(directory).glob(".import-*")), [])

    def test_changed_pdf_during_extraction_is_rejected_without_publishing(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            pdf = root / "source.pdf"
            pdf.write_bytes(b"synthetic")

            def change_source(*_args):
                pdf.write_bytes(b"changed")
                return TEXT.encode()

            with patch.object(manual, "run_bounded", side_effect=change_source):
                with self.assertRaisesRegex(ValueError, "changed during import"):
                    manual.import_manual(pdf, root / "output")
            self.assertFalse((root / "output").exists())

    def test_pdf_size_limit(self):
        with tempfile.TemporaryDirectory() as directory:
            pdf = Path(directory) / "source.pdf"
            pdf.write_bytes(b"too large")
            with patch.object(manual, "MAX_PDF", 4), self.assertRaises(ValueError):
                manual.fingerprint(pdf)

    def test_subprocess_output_deadline_and_exit_status(self):
        self.assertEqual(manual.run_bounded([sys.executable, "-c", "print('ok')"]), b"ok\n")
        with self.assertRaisesRegex(ValueError, "size limit"):
            manual.run_bounded([sys.executable, "-c", "print('12345')"], max_bytes=4)
        start = time.monotonic()
        with self.assertRaisesRegex(ValueError, "time limit"):
            manual.run_bounded([sys.executable, "-c", "import time; time.sleep(3)"], timeout=0.05)
        self.assertLess(time.monotonic() - start, 2)
        with self.assertRaisesRegex(ValueError, "failed to process"):
            manual.run_bounded([sys.executable, "-c", "raise SystemExit(1)"])

    def test_help_needs_no_poppler_and_errors_are_clear(self):
        env = {**os.environ, "PATH": ""}
        help_result = subprocess.run([sys.executable, str(SCRIPT), "--help"], env=env, capture_output=True)
        self.assertEqual(help_result.returncode, 0)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            pdf = root / "synthetic.pdf"
            write_pdf(pdf)
            result = subprocess.run(
                [sys.executable, str(SCRIPT), "--pdf", str(pdf), "--output", str(root / "out")],
                env=env, capture_output=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, b"")
            self.assertIn(b"pdftotext", result.stderr)
            self.assertFalse((root / "out").exists())

    @unittest.skipUnless(shutil.which("pdftotext"), "Install Poppler for synthetic PDF smoke test")
    def test_real_import_with_synthetic_pdf_and_no_shell_interpolation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            pdf = root / "synthetic '$(touch ignored).pdf"
            write_pdf(pdf)
            result = subprocess.run(
                [sys.executable, str(SCRIPT), "--pdf", str(pdf), "--output", str(root / "out")],
                capture_output=True, check=True, timeout=35,
            )
            corpus = Path(json.loads(result.stdout)["corpus"])
            self.assertEqual(corpus.name, hashlib.sha256(pdf.read_bytes()).hexdigest())
            self.assertEqual(len(list((corpus / "pages").glob("*.md"))), 3)
            self.assertIn("Chop Mode", (corpus / "pages/002.md").read_text())
            self.assertIn("printed 99", (corpus / "pages/002.md").read_text())
            self.assertFalse((root / "ignored").exists())
            self.assertEqual(manual.import_manual(pdf, root / "out"), corpus)

    def test_normalized_search_handles_wrapped_words_without_changing_source_pages(self):
        pages = manual.split_pages(TEXT)
        pages[1] = "Grid View\nNote\nlength and ﬁxed duration.\n99\n"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "corpus"
            manual.publish(manual.corpus_files(pages, Path("/synthetic.pdf"), "a" * 64), root)
            before = (root / "pages/002.md").read_bytes()
            manual.prepare_search(pages, "a" * 64, root)
            manual.prepare_search(pages, "a" * 64, root)
            self.assertIn("Note length and fixed duration.", (root / "search/002.txt").read_text())
            self.assertEqual((root / "pages/002.md").read_bytes(), before)
            metadata = json.loads((root / "search/manifest.json").read_text())
            self.assertEqual(metadata["source_sha256"], "a" * 64)
            self.assertEqual(metadata["pdf_pages"], 3)

    def ocr_fixture(self, directory):
        root = Path(directory)
        source = root / "source.pdf"
        source.write_bytes(b"synthetic source bytes; OCR is mocked in unit tests")
        corpus = root / "corpus"
        corpus.mkdir()
        metadata = {"source_sha256": manual.fingerprint(source), "pdf_pages": 3}
        return source, corpus, metadata

    def test_ocr_cache_reuses_verified_pages_without_external_tools_and_repairs_corruption(self):
        with tempfile.TemporaryDirectory() as directory:
            source, corpus, metadata = self.ocr_fixture(directory)
            with patch.object(manual.shutil, "which", return_value="/synthetic"), \
                 patch.object(manual, "run_bounded", return_value=b"tesseract fixture\neng\n"), \
                 patch.object(manual, "ocr_page", return_value="Screenshot-only label") as worker:
                result = manual.prepare_ocr(source, corpus, metadata)
                self.assertEqual(worker.call_count, 3)
                self.assertTrue(result["complete"])
                self.assertEqual(result["completed_pages"], 3)
                self.assertEqual(result["pending_pages"], [])
                worker.reset_mock()
                with patch.object(manual.shutil, "which", side_effect=AssertionError("must reuse cache")):
                    self.assertTrue(manual.prepare_ocr(source, corpus, metadata)["complete"])
                worker.assert_not_called()
                (corpus / "ocr/pages/002.txt").write_text("corrupt")
                self.assertTrue(manual.prepare_ocr(source, corpus, metadata)["complete"])
                self.assertEqual(worker.call_count, 1)
                self.assertEqual(worker.call_args.args[1], 2)
            self.assertIn("OCR SEARCH ONLY", (corpus / "ocr/pages/002.txt").read_text())
            self.assertFalse((corpus / "ocr/.prepare.lock").exists())

    def test_partial_ocr_records_coverage_and_retries_only_failed_pages(self):
        def fail_page_two(_source, number):
            if number == 2:
                raise ValueError("synthetic timeout")
            return "OCR result"

        with tempfile.TemporaryDirectory() as directory:
            source, corpus, metadata = self.ocr_fixture(directory)
            with patch.object(manual.shutil, "which", return_value="/synthetic"), \
                 patch.object(manual, "run_bounded", return_value=b"tesseract fixture\neng\n"):
                with patch.object(manual, "ocr_page", side_effect=fail_page_two):
                    with self.assertRaisesRegex(ValueError, "incomplete"):
                        manual.prepare_ocr(source, corpus, metadata)
                result = json.loads((corpus / "ocr/manifest.json").read_text())
                self.assertFalse(result["complete"])
                self.assertEqual(result["completed_pages"], 2)
                self.assertEqual(result["failed_pages"], [2])
                with patch.object(manual, "ocr_page", return_value="Recovered") as worker:
                    self.assertTrue(manual.prepare_ocr(source, corpus, metadata)["complete"])
                    self.assertEqual(worker.call_count, 1)
                    self.assertEqual(worker.call_args.args[1], 2)

    def test_missing_optional_ocr_tools_preserves_usable_text_search(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source.pdf"
            source.write_bytes(b"synthetic source")
            with patch.object(manual, "run_bounded", return_value=TEXT.encode()), \
                 patch.object(manual.shutil, "which", return_value=None):
                with self.assertRaisesRegex(ValueError, "text/search corpus is already usable"):
                    manual.import_manual(source, root / "output", ocr=True)
            corpus = root / "output" / manual.DOCUMENT_ID / manual.fingerprint(source)
            self.assertIn("Chop Mode", (corpus / "search/002.txt").read_text())
            self.assertFalse(json.loads((corpus / "ocr/manifest.json").read_text())["complete"])

    def test_missing_english_data_fails_before_rendering_pages(self):
        with tempfile.TemporaryDirectory() as directory:
            source, corpus, metadata = self.ocr_fixture(directory)
            with patch.object(manual.shutil, "which", return_value="/synthetic"), \
                 patch.object(manual, "run_bounded", return_value=b"osd\n"), \
                 patch.object(manual, "ocr_page") as worker:
                with self.assertRaisesRegex(ValueError, "English.*eng"):
                    manual.prepare_ocr(source, corpus, metadata)
                worker.assert_not_called()
            self.assertFalse(json.loads((corpus / "ocr/manifest.json").read_text())["complete"])

    def test_ocr_identity_source_and_existing_lock_are_checked(self):
        with tempfile.TemporaryDirectory() as directory:
            source, corpus, metadata = self.ocr_fixture(directory)
            with self.assertRaisesRegex(ValueError, "does not match"):
                manual.prepare_ocr(source, corpus, {**metadata, "source_sha256": "0" * 64})
            lock = corpus / "ocr/.prepare.lock"
            lock.write_text("synthetic lock")
            with self.assertRaisesRegex(ValueError, "already running"):
                manual.prepare_ocr(source, corpus, metadata)
            self.assertEqual(lock.read_text(), "synthetic lock")
            lock.unlink()
            (corpus / "ocr/state.json").write_text(json.dumps({
                "recipe": {"format_version": -1}, "checksums": {}, "errors": {}, "engine_versions": [],
            }))
            with self.assertRaisesRegex(ValueError, "identity/format differs"):
                manual.prepare_ocr(source, corpus, metadata)

    def test_ocr_renders_a_verified_snapshot_not_the_mutable_original(self):
        with tempfile.TemporaryDirectory() as directory:
            source, corpus, metadata = self.ocr_fixture(directory)

            def mutate_original(snapshot, _number):
                self.assertNotEqual(snapshot, source)
                self.assertEqual(manual.fingerprint(snapshot), metadata["source_sha256"])
                source.write_bytes(b"changed original")
                return "OCR of the approved snapshot"

            with patch.object(manual.shutil, "which", return_value="/synthetic"), \
                 patch.object(manual, "run_bounded", return_value=b"tesseract fixture\neng\n"), \
                 patch.object(manual, "ocr_page", side_effect=mutate_original):
                self.assertTrue(manual.prepare_ocr(source, corpus, metadata)["complete"])
            with self.assertRaisesRegex(ValueError, "does not match"):
                manual.prepare_ocr(source, corpus, metadata)

    @unittest.skipUnless(
        all(shutil.which(program) for program in ["pdftotext", "pdftoppm", "tesseract"]),
        "Install Poppler and Tesseract eng for synthetic OCR smoke test",
    )
    def test_real_synthetic_ocr_preparation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            pdf = root / "synthetic.pdf"
            write_pdf(pdf)
            corpus = manual.import_manual(pdf, root / "output", ocr=True)
            result = json.loads((corpus / "ocr/manifest.json").read_text())
            self.assertTrue(result["complete"])
            self.assertEqual(result["completed_pages"], 3)
            self.assertIn("Chop Mode", (corpus / "ocr/pages/002.txt").read_text())
            self.assertEqual(list((corpus / "ocr").rglob("*.png")), [])

    @unittest.skipUnless(os.environ.get("MPC_FORCE_MANUAL"), "Explicit local supplied-manual check only")
    def test_supplied_force_manual(self):
        with tempfile.TemporaryDirectory() as directory:
            pdf = Path(os.environ["MPC_FORCE_MANUAL"])
            corpus = manual.import_manual(pdf, Path(directory))
            manifest = json.loads((corpus / "manifest.json").read_text())
            self.assertEqual(manifest["pdf_pages"], 468)
            self.assertLess((corpus / "index.md").stat().st_size, 2048)
            for query, page in [("Chop Mode", 261), ("Track Mixer", 124), ("Step Sequencer", 173)]:
                text = (corpus / f"pages/{page:03}.md").read_text()
                self.assertIn(query, text)
                self.assertIn(f"PDF p. {page}", text)
            for query in ["Chop Mode", "Track Mixer", "Remote Screen", "Step Sequencer", "DrumSynth"]:
                matches = [path.name for path in (corpus / "pages").glob("*.md") if query.casefold() in path.read_text().casefold()]
                self.assertTrue(matches, query)
                print(f"{query}: {', '.join(sorted(matches)[:12])}")
            image = manual.run_bounded([
                "pdftoppm", "-f", "248", "-l", "248", "-singlefile", "-scale-to", "1400", "-png", str(pdf),
            ], max_bytes=8 * 1024 * 1024, timeout=20)
            self.assertEqual(image[:8], b"\x89PNG\r\n\x1a\n")
            self.assertEqual(max(struct.unpack(">II", image[16:24])), 1400)


if __name__ == "__main__":
    unittest.main()
