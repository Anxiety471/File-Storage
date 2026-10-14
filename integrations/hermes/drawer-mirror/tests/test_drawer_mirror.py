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
    """Records PUTs and answers like Drawer: a second identical write reports new_revision false."""

    puts: list = []
    store: dict = {}

    def log_message(self, *args):  # keep test output quiet
        pass

    def do_PUT(self):
        query = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
        body = self.rfile.read(int(self.headers["Content-Length"]))
        path = query["path"][0]
        FakeDrawer.puts.append({"path": path, "body": body, "headers": {k.lower(): v for k, v in self.headers.items()}})
        changed = FakeDrawer.store.get(path) != body
        FakeDrawer.store[path] = body
        payload = json.dumps({"new_revision": changed, "revision": 1}).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


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
        self._saved = {k: os.environ.get(k) for k in list(self.env) + ["DRAWER_MIRROR_MAX_BYTES"]}
        os.environ.update(self.env)
        os.environ.pop("DRAWER_MIRROR_MAX_BYTES", None)
        FakeDrawer.puts = []
        FakeDrawer.store = {}

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

    def test_register_hooks_post_tool_call(self):
        ctx = Ctx()
        mirror.register(ctx)
        self.assertEqual(ctx.hooks["post_tool_call"], [mirror._on_post_tool_call])

    def test_write_file_is_mirrored_with_bytes_and_author(self):
        path = self.write("reports/plan.md", b"# plan\n")
        mirror._on_post_tool_call(tool_name="write_file", args={"path": path, "content": "x"}, result='{"ok": true}')
        self.assertEqual(len(FakeDrawer.puts), 1)
        put = FakeDrawer.puts[0]
        self.assertEqual(put["path"], "/hermes/reports/plan.md")
        self.assertEqual(put["body"], b"# plan\n")
        self.assertEqual(put["headers"]["x-drawer-author"], "hermes")
        self.assertNotIn("authorization", put["headers"])

    def test_binary_bytes_go_through_untouched(self):
        blob = bytes(range(256)) * 4
        path = self.write("chart.png", blob)
        mirror._on_post_tool_call(tool_name="patch", args={"path": path, "mode": "replace"}, result='{"ok": true}')
        self.assertEqual(FakeDrawer.puts[0]["body"], blob)

    def test_unchanged_rewrite_is_sent_but_adds_no_revision_on_drawer(self):
        path = self.write("a.txt", b"same")
        for _ in range(2):
            mirror._on_post_tool_call(tool_name="write_file", args={"path": path}, result='{"ok": true}')
        self.assertEqual(len(FakeDrawer.puts), 2)
        self.assertEqual(FakeDrawer.store["/hermes/a.txt"], b"same")

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
        path = self.write("ignored.txt", b"x")  # inside the real root, but the root is now refused
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
        self.assertEqual(FakeDrawer.puts[0]["headers"]["authorization"], "Bearer s3cret")

    def test_drawer_down_never_raises(self):
        os.environ["DRAWER_URL"] = "http://127.0.0.1:9"  # nothing listens here
        path = self.write("d.md", b"x")
        mirror._on_post_tool_call(tool_name="write_file", args={"path": path}, result='{"ok": true}')  # must not raise

    def test_relative_path_resolves_against_process_cwd(self):
        cwd = os.getcwd()
        try:
            os.chdir(self.root)
            self.write("rel/one.md", b"1")
            mirror._on_post_tool_call(tool_name="write_file", args={"path": "rel/one.md"}, result='{"ok": true}')
        finally:
            os.chdir(cwd)
        self.assertEqual(FakeDrawer.puts[0]["path"], "/hermes/rel/one.md")


if __name__ == "__main__":
    unittest.main()
