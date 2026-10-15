"""Run with: python3 -m unittest discover -s tests -v   (stdlib only, no Hermes install needed)."""

import importlib.util
import json
import os
import tempfile
import threading
import unittest
import urllib.parse
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

PLUGIN_DIR = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("drawer_mirror", PLUGIN_DIR / "__init__.py")
mirror = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mirror)


class FakeDrawer(BaseHTTPRequestHandler):
    """Answers like Drawer for the calls the plugin makes: GET and PUT on /drawer/v1/files,
    x-drawer-revision on reads, if_head_revision preconditions (412 on mismatch), and no new
    revision when the bytes are unchanged."""

    puts: list = []
    files: dict = {}  # path -> {"body": bytes, "revision": int}
    fail_next_put_with_412 = 0

    def log_message(self, *args):  # keep test output quiet
        pass

    def _send(self, status, body=b"", headers=None):
        self.send_response(status)
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        query = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
        entry = FakeDrawer.files.get(query["path"][0])
        if entry is None:
            return self._send(404, b'{"error":"not found"}')
        self._send(200, entry["body"], {"x-drawer-revision": str(entry["revision"])})

    def do_PUT(self):
        query = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
        body = self.rfile.read(int(self.headers["Content-Length"]))
        path = query["path"][0]
        FakeDrawer.puts.append({"path": path, "body": body,
                                "headers": {k.lower(): v for k, v in self.headers.items()},
                                "if_head_revision": query.get("if_head_revision", [None])[0]})
        current = FakeDrawer.files.get(path, {"body": None, "revision": 0})
        if FakeDrawer.fail_next_put_with_412:
            FakeDrawer.fail_next_put_with_412 -= 1
            return self._send(412, b'{"error":"precondition failed"}')
        if "if_head_revision" in query and int(query["if_head_revision"][0]) != current["revision"]:
            return self._send(412, b'{"error":"precondition failed"}')
        changed = current["body"] != body
        revision = current["revision"] + 1 if changed else current["revision"]
        FakeDrawer.files[path] = {"body": body, "revision": revision}
        payload = json.dumps({"new_revision": changed, "revision": revision}).encode()
        self._send(200, payload, {"content-type": "application/json"})


class Ctx:
    def __init__(self):
        self.hooks = {}

    def register_hook(self, name, fn):
        self.hooks.setdefault(name, []).append(fn)


class MirrorTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = HTTPServer(("127.0.0.1", 0), FakeDrawer)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()
        cls.url = f"http://127.0.0.1:{cls.server.server_port}"

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(self.tmp.name)
        self.home = base / "hermes-home"
        self.root = self.home / "artifacts"
        self.root.mkdir(parents=True)
        self.env = {
            "HERMES_HOME": str(self.home),
            "DRAWER_URL": self.url,
            "DRAWER_MIRROR_ROOT": str(self.root),
            "DRAWER_MIRROR_PREFIX": "/hermes",
            "DRAWER_TOKEN": "",
        }
        self._saved = {k: os.environ.get(k) for k in list(self.env) + ["DRAWER_MIRROR_MAX_BYTES", "DRAWER_MIRROR_LINKS"]}
        os.environ.update(self.env)
        os.environ.pop("DRAWER_MIRROR_MAX_BYTES", None)
        os.environ.pop("DRAWER_MIRROR_LINKS", None)
        FakeDrawer.puts = []
        FakeDrawer.files = {}
        FakeDrawer.fail_next_put_with_412 = 0

    def tearDown(self):
        for k, v in self._saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
        self.tmp.cleanup()

    def write(self, rel, data: bytes):
        p = self.root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data)
        return str(p)

    def file_puts(self):
        return [p for p in FakeDrawer.puts if p["path"] != "/hermes/links.md"]

    def links_text(self):
        return FakeDrawer.files["/hermes/links.md"]["body"].decode()

    # ---- file mirroring -------------------------------------------------------------

    def test_register_hooks_both_observers(self):
        ctx = Ctx()
        mirror.register(ctx)
        self.assertEqual(ctx.hooks["post_tool_call"], [mirror._on_post_tool_call])
        self.assertEqual(ctx.hooks["post_llm_call"], [mirror._on_post_llm_call])

    def test_write_file_is_mirrored_with_bytes_and_author(self):
        path = self.write("reports/plan.md", b"# plan\n")
        mirror._on_post_tool_call(tool_name="write_file", args={"path": path, "content": "x"}, result='{"ok": true}')
        self.assertEqual(len(self.file_puts()), 1)
        put = self.file_puts()[0]
        self.assertEqual(put["path"], "/hermes/reports/plan.md")
        self.assertEqual(put["body"], b"# plan\n")
        self.assertEqual(put["headers"]["x-drawer-author"], "hermes")
        self.assertNotIn("authorization", put["headers"])

    def test_binary_bytes_go_through_untouched(self):
        blob = bytes(range(256)) * 4
        path = self.write("chart.png", blob)
        mirror._on_post_tool_call(tool_name="patch", args={"path": path, "mode": "replace"}, result='{"ok": true}')
        self.assertEqual(self.file_puts()[0]["body"], blob)

    def test_unchanged_rewrite_adds_no_revision_on_drawer(self):
        path = self.write("a.txt", b"same")
        for _ in range(2):
            mirror._on_post_tool_call(tool_name="write_file", args={"path": path}, result='{"ok": true}')
        self.assertEqual(len(self.file_puts()), 2)
        self.assertEqual(FakeDrawer.files["/hermes/a.txt"]["revision"], 1)

    def test_paths_outside_root_are_ignored(self):
        outside = Path(self.tmp.name) / "elsewhere.txt"
        outside.write_text("nope")
        mirror._on_post_tool_call(tool_name="write_file", args={"path": str(outside)}, result='{"ok": true}')
        self.assertEqual(FakeDrawer.puts, [])

    def test_symlink_escaping_root_is_ignored(self):
        secret = Path(self.tmp.name) / "secret.txt"
        secret.write_text("token=abc")
        link = self.root / "link.txt"
        link.symlink_to(secret)
        mirror._on_post_tool_call(tool_name="write_file", args={"path": str(link)}, result='{"ok": true}')
        self.assertEqual(FakeDrawer.puts, [])

    def test_hidden_files_and_folders_stay_local(self):
        env_file = self.write(".env", b"OPENAI_API_KEY=sk-test")
        hidden_dir = self.write(".cache/x.txt", b"x")
        for p in (env_file, hidden_dir):
            mirror._on_post_tool_call(tool_name="write_file", args={"path": p}, result='{"ok": true}')
        self.assertEqual(FakeDrawer.puts, [])

    def test_hermes_home_itself_is_refused_as_root(self):
        os.environ["DRAWER_MIRROR_ROOT"] = str(self.home)
        self.assertIsNone(mirror._settings())
        os.environ["DRAWER_MIRROR_ROOT"] = str(self.home.parent)
        self.assertIsNone(mirror._settings())
        path = self.write("ignored.txt", b"x")
        mirror._on_post_tool_call(tool_name="write_file", args={"path": path}, result='{"ok": true}')
        self.assertEqual(FakeDrawer.puts, [])

    def test_failed_tool_results_are_not_mirrored(self):
        path = self.write("half.md", b"partial")
        mirror._on_post_tool_call(tool_name="write_file", args={"path": path}, result='{"error": "disk full"}')
        self.assertEqual(FakeDrawer.puts, [])

    def test_other_tools_are_ignored(self):
        path = self.write("t.txt", b"x")
        mirror._on_post_tool_call(tool_name="terminal", args={"command": f"echo x > {path}"}, result="{}")
        mirror._on_post_tool_call(tool_name="read_file", args={"path": path}, result="{}")
        self.assertEqual(FakeDrawer.puts, [])

    def test_missing_root_env_disables_mirroring(self):
        os.environ.pop("DRAWER_MIRROR_ROOT")
        path = self.write("x.txt", b"x")
        mirror._on_post_tool_call(tool_name="write_file", args={"path": path}, result='{"ok": true}')
        self.assertEqual(FakeDrawer.puts, [])

    def test_size_cap_skips_large_files(self):
        os.environ["DRAWER_MIRROR_MAX_BYTES"] = "4"
        path = self.write("big.txt", b"0123456789")
        mirror._on_post_tool_call(tool_name="write_file", args={"path": path}, result='{"ok": true}')
        self.assertEqual(FakeDrawer.puts, [])

    def test_token_is_sent_as_bearer_header(self):
        os.environ["DRAWER_TOKEN"] = "s3cret"
        path = self.write("t.md", b"x")
        mirror._on_post_tool_call(tool_name="write_file", args={"path": path}, result='{"ok": true}')
        self.assertEqual(self.file_puts()[0]["headers"]["authorization"], "Bearer s3cret")

    def test_drawer_down_never_raises(self):
        os.environ["DRAWER_URL"] = "http://127.0.0.1:9"  # nothing listens here
        path = self.write("d.md", b"x")
        mirror._on_post_tool_call(tool_name="write_file", args={"path": path}, result='{"ok": true}')
        mirror._on_post_llm_call(assistant_response="see https://example.com/a", session_id="s1")

    def test_relative_path_resolves_against_process_cwd(self):
        cwd = os.getcwd()
        try:
            os.chdir(self.root)
            self.write("rel/one.md", b"1")
            mirror._on_post_tool_call(tool_name="write_file", args={"path": "rel/one.md"}, result='{"ok": true}')
        finally:
            os.chdir(cwd)
        self.assertEqual(self.file_puts()[0]["path"], "/hermes/rel/one.md")

    # ---- link capture ---------------------------------------------------------------

    def test_extract_links_cleans_and_dedupes(self):
        text = ("Release notes: https://github.com/cli/cli/releases/tag/v2.97.0. "
                "Again https://github.com/cli/cli/releases/tag/v2.97.0, "
                "see [docs](https://example.com/guide?token=abc#frag) and (https://en.wikipedia.org/wiki/Foo_(bar)). "
                "Token in url: https://user:pw@internal.example.com/x "
                "local http://127.0.0.1:8787/drawer and http://localhost:3000/ skipped")
        self.assertEqual(mirror._extract_links(text), [
            "https://github.com/cli/cli/releases/tag/v2.97.0",
            "https://example.com/guide",
            "https://en.wikipedia.org/wiki/Foo_(bar",
            "https://internal.example.com/x",
        ])

    def test_reply_links_are_appended_once_with_header(self):
        mirror._on_post_llm_call(assistant_response="Read https://example.com/a and https://example.com/b.",
                                 session_id="sess-1")
        text = self.links_text()
        self.assertTrue(text.startswith("# Links shared by Hermes\n\n"))
        self.assertIn("https://example.com/a", text)
        self.assertIn("https://example.com/b", text)
        self.assertIn("session sess-1", text)
        self.assertEqual(FakeDrawer.files["/hermes/links.md"]["revision"], 1)

    def test_repeat_links_add_nothing_and_new_ones_append(self):
        mirror._on_post_llm_call(assistant_response="https://example.com/a", session_id="s1")
        mirror._on_post_llm_call(assistant_response="again https://example.com/a", session_id="s2")
        self.assertEqual(FakeDrawer.files["/hermes/links.md"]["revision"], 1)
        mirror._on_post_llm_call(assistant_response="new https://example.com/c", session_id="s2")
        self.assertEqual(FakeDrawer.files["/hermes/links.md"]["revision"], 2)
        self.assertEqual(self.links_text().count("https://example.com/a"), 1)
        self.assertIn("https://example.com/c", self.links_text())

    def test_links_write_retries_once_on_precondition_conflict(self):
        FakeDrawer.fail_next_put_with_412 = 1
        mirror._on_post_llm_call(assistant_response="https://example.com/retry", session_id="s")
        self.assertEqual(sum(1 for p in FakeDrawer.puts if p["path"] == "/hermes/links.md"), 2)
        self.assertIn("https://example.com/retry", self.links_text())

    def test_links_write_sends_if_head_revision(self):
        mirror._on_post_llm_call(assistant_response="https://example.com/first", session_id="s")
        mirror._on_post_llm_call(assistant_response="https://example.com/second", session_id="s")
        self.assertEqual([p["if_head_revision"] for p in FakeDrawer.puts if p["path"] == "/hermes/links.md"], ["0", "1"])

    def test_links_can_be_turned_off(self):
        os.environ["DRAWER_MIRROR_LINKS"] = "0"
        mirror._on_post_llm_call(assistant_response="https://example.com/off", session_id="s")
        self.assertEqual(FakeDrawer.puts, [])

    def test_replies_without_links_send_nothing(self):
        mirror._on_post_llm_call(assistant_response="no links here", session_id="s")
        mirror._on_post_llm_call(assistant_response=None, session_id="s")
        self.assertEqual(FakeDrawer.puts, [])


if __name__ == "__main__":
    unittest.main()
