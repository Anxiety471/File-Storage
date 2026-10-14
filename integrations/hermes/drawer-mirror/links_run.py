import importlib.util, logging, os
logging.basicConfig(level=logging.INFO, format="%(name)s %(message)s")
spec = importlib.util.spec_from_file_location("drawer_mirror", "/home/claude/file-storage/integrations/hermes/drawer-mirror/__init__.py")
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
m._on_post_llm_call(assistant_response="Latest gh: https://github.com/cli/cli/releases/tag/v2.97.0 and docs https://example.com/guide?token=SECRET", session_id="sess-e2e")
m._on_post_llm_call(assistant_response="Same again https://github.com/cli/cli/releases/tag/v2.97.0 plus https://example.com/new", session_id="sess-e2e-2")
m._on_post_llm_call(assistant_response="nothing to see", session_id="x")
