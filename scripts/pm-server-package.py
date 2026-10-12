#!/usr/bin/env python3
"""Seal an existing Linux bundle as an explicitly scoped PM candidate."""

import argparse
import hashlib
import json
import shutil
import subprocess
import tarfile
from pathlib import Path


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def inventory(root):
    files = {}
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root).as_posix()
        if path.is_symlink():
            # Reject escaped or dangling dependencies; retain link identity.
            path.resolve(strict=True).relative_to(root.resolve())
            files[relative] = {"symlink": path.readlink().as_posix()}
        elif path.is_file():
            files[relative] = {"sha256": sha256(path), "bytes": path.stat().st_size}
    return files


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[1]
    source = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=repo, text=True
    ).strip()
    subprocess.run(["git", "diff", "--exit-code", "HEAD"], cwd=repo, check=True)
    root = args.bundle.resolve(strict=True)
    assert root.name == "bat-server-linux-x86_64"
    assert not (root / "PM-PACKAGE.json").exists(), "Seal a freshly assembled bundle"
    for name in (
        "bat-server",
        "node-runtime/linux-x86_64/bin/node",
        "node-sidecar/dist/server.mjs",
        "codex-runtime/bin/codex",
    ):
        assert (root / name).is_file(), name
    assert not args.output.exists(), "Use a new output directory"
    args.output.mkdir(parents=True)
    shutil.copyfile(repo / "docs/pm-server-candidate.md", root / "README.md")
    shutil.copyfile(repo / "docs/design/codex-pm-runtime.md", root / "PM-RUNTIME.md")
    # The generic installer stops an existing service and prints its token.
    # This candidate uses the reviewable, versioned manual procedure instead.
    (root / "install.sh").unlink()
    catalog = json.loads((repo / "runtime-catalog.json").read_text())
    manifest = {
        "schema": 1,
        "kind": "bat-pm-server-candidate",
        "target": "linux-x86_64",
        "source_revision": source,
        "source_repository": "https://github.com/teddashh/better-agent-terminal",
        "version": json.loads((repo / "package.json").read_text())["version"],
        "pm_profile": "codex-pm-linux-v1",
        "pm_profile_version": 1,
        "prerequisites": {
            "linux_arch": "x86_64",
            "node_glibc_minimum": "2.28",
            "pm_codex_version": "0.160.0",
            "pm_codex_source": "a956835d020762cb2b570053af06f643a11c0ecc",
            "pm_codex_bundled": False,
            "pm_codex_selection": "BAT_PM_CODEX_PATH or codex on PATH",
            "bubblewrap_with_usable_namespaces": True,
            "existing_regular_codex_auth_file": True,
            "existing_admin_codex_policy": "refused; never remove to bypass",
        },
        "ordinary_bundled_runtimes": {
            key: catalog[key]["version"] for key in ("node", "codex", "claude")
        },
        "evidence": {
            "installed_on_owner_host": False,
            "live_model_acceptance": False,
            "package_smoke_receipt": "package-smoke.json",
        },
        "files": inventory(root),
    }
    manifest_text = json.dumps(manifest, indent=2, sort_keys=True) + "\n"
    (root / "PM-PACKAGE.json").write_text(manifest_text)
    (args.output / "PM-PACKAGE.json").write_text(manifest_text)
    shutil.copyfile(root / "README.md", args.output / "README.md")
    archive = args.output / f"bat-pm-server-linux-x86_64-{source}.tar.gz"
    with tarfile.open(archive, "w:gz") as tar:
        tar.add(root, arcname=root.name)
    (args.output / "SHA256SUMS").write_text(
        "".join(
            f"{sha256(path)}  {path.name}\n"
            for path in (
                archive,
                args.output / "PM-PACKAGE.json",
                args.output / "README.md",
            )
        )
    )
    print(
        json.dumps(
            {
                "source_revision": source,
                "archive": archive.name,
                "sha256": sha256(archive),
            }
        )
    )


if __name__ == "__main__":
    main()
