"""Loopback-only kill/resume regression with real worker processes.

python tests/smoke_resume.py target/release/http-service-scan-gui.exe
"""
import argparse
import http.server
import ipaddress
import json
from pathlib import Path
import queue
import subprocess
import threading
import time

from smoke_worker import Handler


def roundtrip(exe, config, completed, stop_after=None):
    events = queue.Queue()
    children = []
    readers = []

    def read(child):
        for line in child.stdout:
            if line.endswith(b"\n"):
                events.put(json.loads(line))

    try:
        for index in range(config["workers"]):
            child = subprocess.Popen([exe, "--worker"], stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            children.append(child)
            child.stdin.write(json.dumps(dict(config, worker_index=index,
                                              completed_jobs=sorted(completed))).encode())
            child.stdin.close()
            reader = threading.Thread(target=read, args=(child,), daemon=True)
            readers.append(reader)
            reader.start()
        rows = []
        deadline = time.monotonic() + 15
        while True:
            if time.monotonic() > deadline:
                raise AssertionError("worker round exceeded 15 seconds")
            try:
                rows.append(events.get(timeout=0.05))
            except queue.Empty:
                pass
            if stop_after is not None and len(rows) >= stop_after:
                for child in children:
                    if child.poll() is None:
                        child.kill()
                break
            if all(child.poll() is not None for child in children):
                break
        for child in children:
            code = child.wait(timeout=5)
            if stop_after is None:
                assert code == 0, child.stderr.read().decode(errors="replace")
        for reader in readers:
            reader.join(timeout=5)
            assert not reader.is_alive()
        while not events.empty():
            rows.append(events.get_nowait())
        ids = [r["job_id"] for r in rows]
        assert len(ids) == len(set(ids)), "duplicate within a round"
        assert not completed.intersection(ids), "already completed job was scanned again"
        return rows
    finally:
        for child in children:
            if child.poll() is None:
                child.kill()
            child.wait(timeout=5)
            for pipe in [child.stdin, child.stdout, child.stderr]:
                if pipe and not pipe.closed:
                    pipe.close()


class Fixture(http.server.ThreadingHTTPServer):
    def handle_error(self, request, client_address):
        # Disconnects are expected when an in-flight worker is deliberately killed.
        pass


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("exe", nargs="?", default="target/release/http-service-scan-gui.exe")
    args = parser.parse_args()
    exe = str(Path(args.exe).resolve())
    servers = [Fixture(("127.0.0.1", 0), Handler) for _ in range(4)]
    for server in servers:
        server.delay = 0.2
        threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        ip = int(ipaddress.IPv4Address("127.0.0.1"))
        cfg = dict(ranges=[[ip, ip]], ports=[s.server_port for s in servers],
                   schemes=["http", "https"], concurrency=1, timeout_secs=2,
                   body_limit=4096, insecure=True, host="", workers=2,
                   domains=[dict(domain="localhost", ip=None)])
        completed = set()
        all_rows = []
        for _ in range(2):
            rows = roundtrip(exe, cfg, completed, stop_after=1)
            assert rows
            completed.update(r["job_id"] for r in rows)
            all_rows.extend(rows)
        assert len(completed) < 16, "fixture must leave unfinished work"
        # Job IDs remain stable even when process partitioning changes.
        cfg["workers"] = 3
        rows = roundtrip(exe, cfg, completed)
        all_rows.extend(rows)
        completed.update(r["job_id"] for r in rows)
        assert completed == set(range(16))
        endpoints = [(r["record"]["target"], r["record"]["ip"], r["record"]["port"], r["record"]["scheme"]) for r in all_rows]
        assert len(endpoints) == len(set(endpoints)) == 16
        for result in all_rows:
            r = result["record"]
            if r["scheme"] == "http":
                assert r["status"] == 200 and not r["error"], r
        assert roundtrip(exe, cfg, completed) == []
        print("PASS: stop twice, resume remaining jobs, no duplicate/missing results, completed plan stays idle")
    finally:
        for server in servers:
            server.shutdown()
            server.server_close()


if __name__ == "__main__":
    main()
