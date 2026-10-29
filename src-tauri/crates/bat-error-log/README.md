# bat-server fleet error log

The headless CLI installs this best-effort logger. It writes JSON Lines to
`~/.bat/logs/errors.jsonl` on Linux/macOS or
`%LOCALAPPDATA%\BAT\logs\errors.jsonl` on Windows. The machine ID is the hostname;
the version comes from BAT's build version. No Hub credential is needed.

One worker redacts messages using the connector corpus, computes fingerprints,
and aggregates matching fingerprints for 60 seconds before appending a record.
Rotation retains the current 10 MiB file and four numbered backups. Fatal panic
and startup failure records bypass the aggregation delay on the worker.

Server threads only try a bounded queue. Write failures, queue overflow, and
messages larger than 16 KiB are dropped. Pending records can be lost on process
exit; the logger never waits for disk, even during a panic. The pending map is
bounded to 256 fingerprints (overflow flushes the current aggregates early).
Context is deliberately empty so arbitrary session payloads cannot leak into
this spool. Existing debug logging and IPC contracts are unchanged.

Run unit tests without starting the server:

```sh
cargo test --manifest-path src-tauri/Cargo.toml -p bat-error-log
```

`tests/fixture.json` is copied from the connector's shared error logging corpus.
