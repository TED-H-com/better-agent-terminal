#!/usr/bin/env python3
"""Run only an extracted candidate, on loopback with disposable disk state.

No session start, prompt, model, real credentials, installer or system service.
The small WebSocket client implements only bounded uncompressed text readback.
"""

import argparse
import base64
import hashlib
import json
import os
import secrets
import signal
import socket
import ssl
import struct
import subprocess
import tarfile
import tempfile
import time
from pathlib import Path


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


class Readback:
    def __init__(self, port):
        # This connection is only to our own disposable loopback listener.
        # The actual generated certificate is retained and compared on restart.
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        context.check_hostname = False
        context.verify_mode = ssl.CERT_NONE
        self.sock = context.wrap_socket(
            socket.create_connection(("127.0.0.1", port), timeout=2),
            server_hostname="localhost",
        )
        self.certificate = hashlib.sha256(
            self.sock.getpeercert(binary_form=True)
        ).hexdigest()
        try:
            key = base64.b64encode(secrets.token_bytes(16)).decode()
            request = (
                f"GET / HTTP/1.1\r\nHost: localhost:{port}\r\nUpgrade: websocket\r\n"
                f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
            )
            self.sock.sendall(request.encode())
            header = b""
            while not header.endswith(b"\r\n\r\n"):
                header += self.exact(1)
                assert len(header) <= 16384, "Oversized upgrade header"
            lines = header.decode("ascii").split("\r\n")
            assert lines[0].startswith("HTTP/1.1 101 "), "Upgrade refused"
            fields = {
                name.lower(): value.strip()
                for name, value in (
                    line.split(":", 1) for line in lines[1:] if ":" in line
                )
            }
            expected = base64.b64encode(
                hashlib.sha1(
                    (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()
                ).digest()
            ).decode()
            assert fields["sec-websocket-accept"].strip() == expected
        except BaseException:
            self.sock.close()
            raise

    def exact(self, count):
        chunks = bytearray()
        while len(chunks) < count:
            block = self.sock.recv(count - len(chunks))
            if not block:
                raise EOFError("Readback connection closed")
            chunks.extend(block)
        return bytes(chunks)

    def send(self, payload, opcode=1):
        assert len(payload) < 65536
        mask = secrets.token_bytes(4)
        size = (
            bytes([0x80 | len(payload)])
            if len(payload) < 126
            else b"\xfe" + struct.pack("!H", len(payload))
        )
        self.sock.sendall(
            bytes([0x80 | opcode])
            + size
            + mask
            + bytes(value ^ mask[i % 4] for i, value in enumerate(payload))
        )

    def request(self, frame):
        self.send(json.dumps(frame).encode())
        deadline = time.monotonic() + 10
        for _ in range(64):
            self.sock.settimeout(max(0.01, deadline - time.monotonic()))
            first, second = self.exact(2)
            assert first & 0x80 and not first & 0x70 and not second & 0x80
            size = second & 127
            if size == 126:
                size = struct.unpack("!H", self.exact(2))[0]
            elif size == 127:
                size = struct.unpack("!Q", self.exact(8))[0]
            assert size <= 1024 * 1024
            payload = self.exact(size)
            if first & 15 == 9:
                self.send(payload, opcode=10)
                continue
            assert first & 15 == 1, "Unexpected readback frame"
            response = json.loads(payload)
            if response.get("id") == frame["id"]:
                return response
        raise AssertionError("Readback response limit")

    def close(self):
        self.sock.close()


def stop_owned(process):
    if process is None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=5)
    # Also reap any remaining members of this test-owned process group.
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--delivery", type=Path, required=True)
    parser.add_argument("--scratch", type=Path, required=True)
    args = parser.parse_args()
    args.scratch.mkdir(parents=True, exist_ok=True)
    filesystem = subprocess.check_output(
        ["stat", "-f", "-c", "%T", str(args.scratch)], text=True
    ).strip()
    assert filesystem not in ("tmpfs", "ramfs"), "Scratch must be disk backed"
    delivery = args.delivery.resolve(strict=True)
    for line in (delivery / "SHA256SUMS").read_text().splitlines():
        expected, name = line.split("  ", 1)
        assert Path(name).name == name
        assert sha256(delivery / name) == expected, "Delivery checksum mismatch"
    manifest = json.loads((delivery / "PM-PACKAGE.json").read_text())
    receipt = {
        "schema": 1,
        "source_revision": manifest["source_revision"],
        "evidence_level": "packaged-loopback-and-restart",
        "live_model_acceptance": False,
        "installed_on_owner_host": False,
        "started_agent_sessions": 0,
        "checks": {},
    }
    checks = receipt["checks"]
    with tempfile.TemporaryDirectory(
        prefix="bat-pm-package-", dir=args.scratch
    ) as scratch:
        scratch = Path(scratch)
        archive = (
            delivery
            / f"bat-pm-server-linux-x86_64-{manifest['source_revision']}.tar.gz"
        )
        with tarfile.open(archive) as tar:
            tar.extractall(scratch, filter="data")
        bundle = scratch / "bat-server-linux-x86_64"
        assert json.loads((bundle / "PM-PACKAGE.json").read_text()) == manifest
        for name, info in manifest["files"].items():
            path = bundle / name
            path.resolve(strict=True).relative_to(bundle)
            if "symlink" in info:
                assert (
                    path.is_symlink() and path.readlink().as_posix() == info["symlink"]
                )
            else:
                assert not path.is_symlink() and path.stat().st_size == info["bytes"]
                assert sha256(path) == info["sha256"]
        checks["extracted_payload_hashes"] = True
        home = scratch / "home"
        home.mkdir()
        state = scratch / "state"
        state.mkdir(mode=0o700)
        token = secrets.token_hex(32)
        token_file = scratch / "token"
        token_file.write_text(token + "\n")
        token_file.chmod(0o600)
        env = {
            "HOME": str(home),
            "PATH": "/usr/bin:/bin",
            "TMPDIR": str(scratch),
            "XDG_CONFIG_HOME": str(home / ".config"),
            "XDG_DATA_HOME": str(home / ".local/share"),
            "BAT_PM_CODEX_PATH": str(scratch / "intentionally-unavailable-codex"),
        }
        binary = bundle / "bat-server"
        help_text = subprocess.check_output(
            [str(binary), "--help"], env=env, cwd=bundle, timeout=10, text=True
        )
        assert "--token-file" in help_text and "--data-dir" in help_text
        node = bundle / "node-runtime/linux-x86_64/bin/node"
        assert (
            subprocess.check_output(
                [str(node), "--version"], env=env, timeout=10, text=True
            ).strip()
            == "v" + manifest["ordinary_bundled_runtimes"]["node"]
        )
        subprocess.run(
            [str(node), "--check", str(bundle / "node-sidecar/dist/server.mjs")],
            env=env,
            timeout=20,
            check=True,
        )
        codex_version = subprocess.check_output(
            [str(bundle / "codex-runtime/bin/codex"), "--version"],
            env=env,
            timeout=10,
            text=True,
        ).strip()
        assert (
            codex_version
            == "codex-cli " + manifest["ordinary_bundled_runtimes"]["codex"]
        )
        checks["bundled_executables"] = True
        certificate = None
        for cycle in range(2):
            with socket.socket() as reservation:
                reservation.bind(("127.0.0.1", 0))
                port = reservation.getsockname()[1]
            process = None
            connection = None
            try:
                # The startup banner contains the disposable token; never publish it.
                process = subprocess.Popen(
                    [
                        str(binary),
                        "--bind=localhost",
                        f"--port={port}",
                        f"--data-dir={state}",
                        f"--token-file={token_file}",
                    ],
                    cwd=bundle,
                    env=env,
                    start_new_session=True,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )
                deadline = time.monotonic() + 30
                while connection is None:
                    assert process.poll() is None, (
                        "Packaged server exited before readback"
                    )
                    try:
                        connection = Readback(port)
                    except (ConnectionRefusedError, TimeoutError):
                        if time.monotonic() >= deadline:
                            raise AssertionError("Packaged server startup timed out")
                        time.sleep(0.1)
                auth = connection.request(
                    {
                        "type": "auth",
                        "id": "package-auth",
                        "token": token,
                        "protocols": ["bat-remote/v2"],
                    }
                )
                assert (
                    auth.get("result") is True and auth["protocol"] == "bat-remote/v2"
                )
                assert auth["serverVersion"] == manifest["version"]
                response = connection.request(
                    {
                        "type": "invoke",
                        "id": "package-read",
                        "channel": "claude:get-pm-runtime",
                        "params": {"sessionId": "package-never-started"},
                    }
                )
                assert (
                    response["type"] == "invoke-error"
                    and "PM_RUNTIME_RECEIPT_UNAVAILABLE" in response["error"]
                )
                assert not (state / "pm-runtimes").exists(), (
                    "Readback must not create PM ownership"
                )
                if certificate is None:
                    certificate = connection.certificate
                else:
                    assert certificate == connection.certificate, (
                        "Restart changed server identity"
                    )
                connection.close()
                connection = Readback(port)
                refusal = connection.request(
                    {
                        "type": "auth",
                        "id": "package-wrong-auth",
                        "token": secrets.token_hex(32),
                        "protocols": ["bat-remote/v2"],
                    }
                )
                assert (
                    refusal.get("error") == "Invalid token"
                    and refusal.get("result") is not True
                )
            finally:
                if connection is not None:
                    connection.close()
                stop_owned(process)
        checks.update(
            {
                "authenticated_loopback": True,
                "wrong_token_refused": True,
                "restart_identity_and_token_preserved": True,
                "missing_pm_receipt_stays_unknown": True,
                "readback_creates_no_pm_session": True,
                "owned_processes_stopped": True,
            }
        )
    checks["disk_scratch_removed"] = not scratch.exists()
    receipt["status"] = "passed"
    (delivery / "package-smoke.json").write_text(json.dumps(receipt, indent=2) + "\n")
    with (delivery / "SHA256SUMS").open("a") as sums:
        sums.write(f"{sha256(delivery / 'package-smoke.json')}  package-smoke.json\n")
    print(json.dumps(receipt))


if __name__ == "__main__":
    main()
