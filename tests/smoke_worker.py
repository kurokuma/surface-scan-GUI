"""Loopback-only multi-process smoke test, or fixture server for manual GUI QA.

python tests/smoke_worker.py target/release/http-service-scan-gui.exe
python tests/smoke_worker.py --serve
"""
import argparse
import base64
import hashlib
import http.server
import ipaddress
import json
from pathlib import Path
import subprocess
import threading
import time

BODY = '<html><title>Local Fixture / 日本語</title><link rel="icon" href="/icon.ico"></html>'.encode()
ICON = b"\x00\x00\x01\x00fingerprint-fixture"


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        time.sleep(getattr(self.server, "delay", 0))
        body = ICON if self.path == "/icon.ico" else BODY
        self.send_response(200)
        self.send_header("Content-Type", "image/x-icon" if self.path == "/icon.ico" else "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Set-Cookie", "fixture=one; HttpOnly")
        self.send_header("Set-Cookie", "second=two; SameSite=Lax")
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("exe", nargs="?", default="target/release/http-service-scan-gui.exe")
    parser.add_argument("--serve", action="store_true")
    parser.add_argument("--delay", type=float, default=0)
    args = parser.parse_args()
    servers = [http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler) for _ in range(2)]
    for server in servers:
        server.delay = args.delay
        threading.Thread(target=server.serve_forever, daemon=True).start()
    ports = [s.server_port for s in servers]
    print(json.dumps({"ip": "127.0.0.1", "ports": ports}), flush=True)
    try:
        if args.serve:
            threading.Event().wait()
            return
        ip = int(ipaddress.IPv4Address("127.0.0.1"))
        cfg = dict(ranges=[[ip, ip]], ports=ports, schemes=["http", "https"], concurrency=2,
                   timeout_secs=2, body_limit=4096, insecure=True, host="", workers=2)
        children = [subprocess.Popen([str(Path(args.exe).resolve()), "--worker"],
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE) for _ in range(2)]
        for i, child in enumerate(children):
            child.stdin.write(json.dumps(dict(cfg, worker_index=i)).encode())
            child.stdin.close()
            child.stdin = None
        rows = []
        for child in children:
            stdout, stderr = child.communicate(timeout=15)
            assert child.returncode == 0, stderr.decode(errors="replace")
            rows.extend(json.loads(line)["record"] for line in stdout.splitlines())
        assert len(rows) == 4
        assert len({(r["ip"], r["port"], r["scheme"]) for r in rows}) == 4
        for row in rows:
            if row["scheme"] == "http":
                assert row["status"] == 200 and not row["error"], row
                assert row["title"] == "Local Fixture / 日本語"
                assert row["body_sha256"] == hashlib.sha256(BODY).hexdigest()
                assert base64.b64decode(row["body_base64"]) == BODY
                assert row["favicon_sha256"] == hashlib.sha256(ICON).hexdigest()
                assert len(row["cookies"]) == 2
            else:
                assert row["error"] and row["tls_error"]
        print("PASS: two release workers, exact job partition, HTTP fingerprints, HTTPS errors", flush=True)
    finally:
        for server in servers:
            server.shutdown()
            server.server_close()


if __name__ == "__main__":
    main()
