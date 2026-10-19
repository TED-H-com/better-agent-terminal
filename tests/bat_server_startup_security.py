"""Isolated Linux CLI acceptance: no credentials in diagnostics; auth survives restart.

Run after a headless build: python3 tests/bat_server_startup_security.py PATH_TO_BINARY
Only temporary loopback servers and synthetic credentials are used.
"""
import base64
import json
import os
from pathlib import Path
import platform
import pty
import select
import socket
import ssl
import struct
import subprocess
import sys
import tempfile
import time


BINARY = str(Path(sys.argv[1]).resolve())
ROOT = Path(__file__).resolve().parents[1]
TOKEN = "startup-fixture-credential-0123456"


def read_until(fd, marker, timeout=20):
    result = b""
    deadline = time.monotonic() + timeout
    while marker not in result and time.monotonic() < deadline:
        if select.select([fd], [], [], 0.1)[0]:
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                break
            if not chunk:
                break
            result += chunk
    assert marker in result, "server did not produce expected startup output"
    return result


def unused_port():
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        return reservation.getsockname()[1]


def authenticate(port, token):
    # Trust only this test's local self-signed fixture server, never a real host.
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    context.check_hostname = False
    context.verify_mode = ssl.CERT_NONE
    with socket.create_connection(("127.0.0.1", port), timeout=5) as tcp:
        with context.wrap_socket(tcp, server_hostname="localhost") as stream:
            key = base64.b64encode(os.urandom(16)).decode()
            stream.sendall((f"GET / HTTP/1.1\r\nHost: localhost:{port}\r\n"
                            "Upgrade: websocket\r\nConnection: Upgrade\r\n"
                            f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
            headers = b""
            while not headers.endswith(b"\r\n\r\n"):
                headers += stream.recv(1)
            assert headers.startswith(b"HTTP/1.1 101"), "WebSocket upgrade failed"
            frame = json.dumps({"type": "auth", "id": "fixture", "token": token}).encode()
            mask = os.urandom(4)
            assert len(frame) < 126
            stream.sendall(bytes([0x81, 0x80 | len(frame)]) + mask +
                           bytes(byte ^ mask[i % 4] for i, byte in enumerate(frame)))

            def read_exact(size):
                data = b""
                while len(data) < size:
                    chunk = stream.recv(size - len(data))
                    assert chunk, "connection closed before authentication result"
                    data += chunk
                return data

            for _ in range(10):
                header = read_exact(2)
                length = header[1] & 127
                if length == 126:
                    length = struct.unpack("!H", read_exact(2))[0]
                elif length == 127:
                    length = struct.unpack("!Q", read_exact(8))[0]
                assert not header[1] & 128
                response = json.loads(read_exact(length))
                if response.get("type") == "auth-result":
                    assert response.get("result") is True, "persisted token did not authenticate"
                    return
            raise AssertionError("missing authentication response")


with tempfile.TemporaryDirectory(prefix="bat-startup-security-") as scratch:
    scratch = Path(scratch)
    data = scratch / "data"
    catalog = json.loads((ROOT / "runtime-catalog.json").read_text())
    arch = {"x86_64": "x64", "aarch64": "arm64"}[platform.machine()]
    codex = data / "runtimes" / "codex" / catalog["codex"]["version"] / f"linux-{arch}" / "bin" / "codex"
    codex.parent.mkdir(parents=True)
    codex.write_text("#!/bin/sh\necho codex-fixture\n")
    codex.chmod(0o700)  # Prevent startup's optional runtime download.
    token_file = scratch / "token"
    token_file.write_text(TOKEN)
    token_file.chmod(0o600)
    env = {"PATH": os.environ["PATH"], "HOME": str(scratch), "TMPDIR": str(scratch),
           "XDG_CONFIG_HOME": str(scratch / "config"), "BAT_DEBUG": "1"}
    common = [BINARY, "--bind=localhost", f"--data-dir={data}"]
    refused = subprocess.run(common + ["--print-connect-link"], env=env, capture_output=True, timeout=10)
    assert refused.returncode == 1
    assert b"requires stdout to be a terminal" in refused.stderr
    assert TOKEN.encode() not in refused.stdout + refused.stderr
    assert not (data / "server-token.enc.json").exists(), "refusal must precede server startup"
    # Error paths must not echo a full connection URL supplied as an argument.
    rejected = subprocess.run(common + [f"wss://example.invalid?token={TOKEN}&fp=AA"],
                              env=env, capture_output=True, timeout=10)
    assert rejected.returncode == 1
    assert TOKEN.encode() not in rejected.stdout + rejected.stderr
    assert b"unexpected argument" in rejected.stderr
    assert b"example.invalid" not in rejected.stderr
    rejected = subprocess.run(common + [f"--unknown-credential={TOKEN}"],
                              env=env, capture_output=True, timeout=10)
    assert rejected.returncode == 1
    assert TOKEN.encode() not in rejected.stdout + rejected.stderr

    for restart in [False, True]:
        port = unused_port()
        args = common + [f"--port={port}"]
        if not restart:
            args.append(f"--token-file={token_file}")
        with tempfile.TemporaryFile() as errors:
            process = subprocess.Popen(args, env=env, stdout=subprocess.PIPE, stderr=errors)
            try:
                output = read_until(process.stdout.fileno(), b"  connect:")
                authenticate(port, TOKEN)
            finally:
                process.terminate()
                rest, _ = process.communicate(timeout=10)
            errors.seek(0)
            assert TOKEN.encode() not in output + rest + errors.read()
        assert b"hidden" in output and b"server-token.enc.json" in output
        assert b"token=[REDACTED]" in output + rest
        persisted = data / "server-token.enc.json"
        assert persisted.is_file()
        for log in (data / "logs").glob("*.log"):
            assert TOKEN not in log.read_text(), "credential appeared in server diagnostic log"

    # A real pseudo-terminal exercises the OS terminal check as well as opt-in.
    master, slave = pty.openpty()
    with tempfile.TemporaryFile() as errors:
        process = subprocess.Popen(common + [f"--token-file={token_file}", f"--port={unused_port()}",
                                            "--print-connect-link"],
                                   env=env, stdout=slave, stderr=errors)
        os.close(slave)
        try:
            output = read_until(master, b"full-connect:")
            if TOKEN.encode() not in output:
                output += read_until(master, TOKEN.encode())
            assert TOKEN.encode() in output
        finally:
            process.terminate()
            process.wait(timeout=10)
            os.close(master)
        errors.seek(0)
        assert TOKEN.encode() not in errors.read()
    for log in (data / "logs").glob("*.log"):
        assert TOKEN not in log.read_text()

print("Startup security: journal, debug logs, redirected refusal, TTY opt-in, token-file auth and persisted-token restart passed")
