#!/usr/bin/env python3
"""Verify the exact upstream crate and the two-line GTK3 security backport."""
import hashlib
import io
from pathlib import Path
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
VENDOR = ROOT / "src-tauri/vendor/glib-0.18.5"
URL = "https://static.crates.io/crates/glib/glib-0.18.5.crate"
EXPECTED = "233daaf6e83ae6a12a52055f568f9d7cf4671dabb78ff9560ab6da230ce00ee5"

with urllib.request.urlopen(URL, timeout=60) as response:
    archive = response.read()
assert hashlib.sha256(archive).hexdigest() == EXPECTED, "Upstream checksum mismatch"
expected_paths = {"PROVENANCE.md", "SECURITY-PATCH.md"}
with tarfile.open(fileobj=io.BytesIO(archive)) as source:
    for member in source.getmembers():
        if not member.isfile():
            continue
        relative = member.name.split("/", 1)[1]
        expected_paths.add(relative)
        expected = source.extractfile(member).read()
        if relative == "src/variant_iter.rs":
            for old, new in [
                (b"let p: *mut libc::c_char = std::ptr::null_mut();", b"let mut p: *mut libc::c_char = std::ptr::null_mut();"),
                (b"                &p,", b"                &mut p,"),
            ]:
                assert expected.count(old) == 1, "Upstream patch context changed"
                expected = expected.replace(old, new)
        assert (VENDOR / relative).read_bytes() == expected, f"Unexpected change: {relative}"
actual_paths = {str(path.relative_to(VENDOR)) for path in VENDOR.rglob("*") if path.is_file()}
assert actual_paths == expected_paths, "Unexpected or missing vendored files"
print("Verified glib 0.18.5 checksum and exact upstream iterator fix.")
