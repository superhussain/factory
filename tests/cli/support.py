import contextlib
import importlib.util
import io
import os
import stat
import tempfile
import unittest
import hashlib
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SKILL = ROOT / "skills" / "factory"


def load_runtime():
    spec = importlib.util.spec_from_file_location("factory", SKILL / "runtime/factory.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class IsolatedTestCase(unittest.TestCase):
    def run(self, result=None):
        with tempfile.TemporaryDirectory() as temp:
            home = Path(temp)
            env = dict(os.environ)
            for key in list(env):
                if (key in ("FACTORY_HOME", "FACTORY_SESSION", "PASEO_HOME", "PASEO_AGENT_ID",
                            "GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_INDEX_FILE")
                        or key.startswith("GIT_CONFIG_") or key.lower().endswith("_proxy")):
                    env.pop(key, None)
            env.update(HOME=temp, USERPROFILE=temp, XDG_CONFIG_HOME=str(home / "config"),
                       NO_PROXY="localhost,127.0.0.1",
                       GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=str(home / "gitconfig"),
                       GIT_AUTHOR_NAME="Test", GIT_AUTHOR_EMAIL="test@example.invalid",
                       GIT_COMMITTER_NAME="Test", GIT_COMMITTER_EMAIL="test@example.invalid")
            with patch.dict(os.environ, env, clear=True):
                return super().run(result)


def snapshot(root):
    return {str(path.relative_to(root)): (stat.S_IMODE(path.stat().st_mode),
            path.read_bytes() if path.is_file() else None)
            for path in [root] + sorted(root.rglob("*"))}


def invoke(module, args):
    stdout, stderr = io.StringIO(), io.StringIO()
    code = 0
    with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
        try:
            module.main(args)
        except SystemExit as exc:
            code = exc.code
    return code, stdout.getvalue(), stderr.getvalue()


@contextlib.contextmanager
def http_fixture(routes):
    requests = []
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass
        def do_GET(self):
            data = self.rfile.read(int(self.headers.get("Content-Length") or 0))
            requests.append((self.command, self.path, dict(self.headers), data))
            status, body, *extra = routes.get(f"{self.command} {self.path}", routes.get(self.path, (404, b"")))
            if not isinstance(body, bytes):
                body = json.dumps(body).encode("utf-8")
            self.send_response(status)
            if status == 302:
                self.send_header("Location", "https://example.invalid/login")
            self.send_header("ETag", '"' + hashlib.sha256(body).hexdigest() + '"')
            for name, value in (extra[0] if extra else {}).items():
                self.send_header(name, value)
            self.end_headers()
            self.wfile.write(body)
        do_POST = do_PUT = do_GET
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.requests = requests
    server.origin = "http://127.0.0.1:" + str(server.server_port)
    thread = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.01}, daemon=True)
    thread.start()
    try:
        yield server
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
