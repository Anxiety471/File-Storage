import contextlib
import importlib.util
import io
import os
import sqlite3
import tempfile
import unittest
from argparse import Namespace
from pathlib import Path

PLUGIN_DIR = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("drawer_mirror_sync", PLUGIN_DIR / "sync.py")
sync = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sync)


class FakeMirror:
    """Stands in for the plugin module: records what sync.py asks Drawer to do."""

    def __init__(self, cfg):
        self.cfg = cfg
        self.mirrored = []
        self.recorded = []

    def _settings(self):
        return self.cfg

    def _mirror_file(self, path, tool_name, cfg):
        self.mirrored.append(Path(path).name)

    def _extract_links(self, text):
        return [word for word in text.split() if word.startswith("http")]

    def _record_links(self, urls, session_id, cfg):
        self.recorded.append(list(urls))


def make_cfg(root, links=True):
    return {"root": root.resolve(), "prefix": "/hermes", "max_bytes": 1 << 20,
            "links": links, "url": "http://127.0.0.1:1", "token": "", "author": "test"}


def make_args(**overrides):
    values = {"roots": None, "no_links": False, "dry_run": False, "quiet": True, "verbose": False}
    values.update(overrides)
    return Namespace(**values)


class BackfillTests(unittest.TestCase):
    def test_backfill_skips_secrets_dependencies_hidden_and_symlinks(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            (root / "notes.md").write_text("keep me")
            (root / "id_rsa").write_text("secret")
            (root / "server.pem").write_text("secret")
            (root / ".env").write_text("DRAWER_TOKEN=secret")
            (root / "node_modules").mkdir()
            (root / "node_modules" / "lib.md").write_text("dependency")
            (root / ".git").mkdir()
            (root / ".git" / "config.md").write_text("git")
            os.symlink(root / "notes.md", root / "link.md")
            mirror = FakeMirror(make_cfg(root))

            rc = sync._backfill(mirror, make_args())

            self.assertEqual(rc, 0)
            self.assertEqual(mirror.mirrored, ["notes.md"])


class ScanTests(unittest.TestCase):
    def test_symlink_candidates_are_dropped_before_resolving(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            real = root / "report.md"
            real.write_text("report")
            link = root / "latest.md"
            os.symlink(real, link)

            found = sync._candidate_files([root], {str(link): None, str(real): None})

            self.assertEqual(list(found), [real])

    def test_scan_respects_links_opt_out(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            original = sync._hermes_references
            sync._hermes_references = lambda: ({}, {"https://example.test/a": None})
            try:
                off = FakeMirror(make_cfg(root, links=False))
                sync._scan_hermes(off, make_args(roots=[str(root)]))
                on = FakeMirror(make_cfg(root, links=True))
                sync._scan_hermes(on, make_args(roots=[str(root)]))
            finally:
                sync._hermes_references = original

            self.assertEqual(off.recorded, [])
            self.assertEqual(on.recorded, [["https://example.test/a"]])

    def test_runtime_logs_are_skipped(self):
        home = sync.HERMES_HOME
        self.assertEqual(sync._skip_reason(home / "logs" / "agent.log"), "Hermes runtime log")
        self.assertEqual(sync._skip_reason(home / "profiles" / "planner" / "logs" / "run.log"),
                         "Hermes runtime log")
        self.assertIsNone(sync._skip_reason(home / "cache" / "scratch" / "notes.md"))
        self.assertIsNone(sync._skip_reason(home / "artifacts" / "report.md"))

    def test_schema_drift_is_reported_not_silent(self):
        with tempfile.TemporaryDirectory() as tmp:
            db = Path(tmp) / "state.db"
            conn = sqlite3.connect(db)
            conn.execute("create table something_else (x)")
            conn.commit()
            conn.close()
            original = sync.STATE_DB
            sync.STATE_DB = db
            stderr = io.StringIO()
            try:
                with contextlib.redirect_stderr(stderr):
                    files, urls = sync._hermes_references()
            finally:
                sync.STATE_DB = original

            self.assertEqual((files, urls), ({}, {}))
            self.assertIn("cannot read artifact references", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
