# Upstream wire differential

A client that never uses `bac` must see the same `bat-remote/v2` behavior from
our host and the recorded upstream release. This suite runs one client script
against both real headless Rust servers. Unknown fields, missing frames, error
codes, capabilities and event order are compared; the allowlist records existing
compatibility debt, **not permission to add more upstream-channel extensions**.
No upstream source, package file, lock file or existing workflow is changed.

## Run

From the repository root, with the existing Node 24, Rust and installed sidecar
dependencies (no install is performed by the runner):

```sh
node bac/tests/differential/self-test.mjs
node bac/tests/differential/run.mjs \
  --scratch "$HOME/agent-work/tmp/dif" --report "$BAC_EVIDENCE/comparison"
node bac/tests/differential/run.mjs --same-binary --repeat 3 \
  --scratch "$HOME/agent-work/tmp/dif" --report "$BAC_EVIDENCE/stability"
```

The default reference is `bac/UPSTREAM_BASE.json`. Override it with
`--base path/to/reference.json` with the same repository/tag/commit shape.
The referenced Git object must already exist locally; the runner never fetches.
CI supplies a separate checkout through `--upstream-source DIRECTORY`, verifies
its HEAD against the recorded SHA, and archives it. Local source extraction uses
`git archive` followed by `tar -xf`, without changing any checkout or Git ref.

Both archive copies are under the scratch directory. Each runs the upstream
`scripts/build-version.js --skip-build` with `VERSION` set to the recorded tag,
then, in its `src-tauri/` directory:

```sh
cargo build --locked --no-default-features --features headless --bin bat-server
```

This is the binary/feature combination declared in `src-tauri/Cargo.toml` and
used by the server packaging workflow. Release stamping changes only disposable
copies. Every successful auth must report the recorded version (e.g. `3.2.18`),
which is **never normalized**. Local sidecars run their respective archived
source handlers against the already installed dependency tree. CI installs
frozen sidecar dependencies in each checkout separately.

A successful build is cached by source SHA, release tag, `rustc --version` and
build-recipe version. A second run skips Cargo, as recorded in `builds.json`.
Uncommitted changes to tracked runtime source are rejected to avoid silently
testing a stale HEAD. Commit them before running. `--build-only` prepares the
pair. For debugging, `--upstream-bin`, `--upstream-source`, `--fork-bin` and
`--fork-source` accept an existing version-stamped pair; these bypass build
provenance checks, so do not use them in the CI compatibility gate.

Use `--scenarios handshake,profiles` for iteration. The full allowlist is still
checked: a subset comparison fails for unused allowances outside that subset.
`--same-binary` always uses an empty allowlist and requires zero differences.
Use a new report directory for each invocation to avoid confusing old artifacts
with the current run. The build cache can be removed after a round:

```sh
rm -rf "$HOME/agent-work/tmp/dif"
```

Only do this when no run is active, and reserve that directory for this task.

## Isolation and fixtures

Each scenario starts a fresh server, reserved loopback port, random token,
private token file, data directory, home and deterministic temporary Git repo.
The CLI's `--bind=localhost` means IPv4 `127.0.0.1`. The runner connects only
after its own child announces that exact port; it never probes a pre-existing
listener. No inherited provider credentials or real Claude/Codex home reach the
child. SIGTERM and finally blocks stop owned process groups and remove runtime
scratch. Never point this suite at a live host or its data.

Node built-ins are sufficient: `tls`, `crypto`, `zlib`, `net` and child processes.
There is no undeclared `ws` dependency and no additional crate or package. The
transport follows `remote_client.rs` and `remote_core.rs`: HTTP upgrade, masked
WebSocket client frames, v2 auth, named `params` and desktop `args`, and the
`BATGZIP1\0` gzip envelope. It handles fragmented server messages and control
frames. Transport boundary self-tests cover 125, 126, 127, 65535 and 65536 bytes.
TLS certificate verification is disabled only for the owned loopback test
server's disposable self-signed certificate; this suite does not test pinning.

The real sidecar dispatcher is loaded with the SDK override seam used by
`node-sidecar/tests/fast-mode.test.mjs`. Fake turns yield fixed init/result
messages and never execute the SDK. Fixture stderr messages are forwarded through
the same JSON-line event pipe, preserving their content while removing the race
between Rust's separate stdout/stderr reader threads. Small delays in the fake
make the send acknowledgement precede completion. This tests log content, not
production stderr-vs-event scheduling.

The Codex executable adapts the JSON-line fake in
`codex_pm_runtime.rs::tests::FAKE` to ordinary `initialize`, `thread/start`,
`thread/resume`, `thread/read` and `turn/start`. It emits fixed synthetic items;
unknown methods return errors. Its `--version` response also satisfies the
headless startup check, preventing automatic runtime downloads. The fake remains
in the owned process group. Retained PM fixtures create only the hashed ownership
directory that the fork checks; they do not launch PM, use credentials, or claim
to validate a complete persisted receipt.

## Comparison and evidence

Each scenario records the HTTP upgrade response and every sent/received logical
WebSocket frame. JSON is decoded before comparison, object keys are canonicalized,
and **arrays remain ordered**. No event sorting or unknown-field filtering is
performed. Compressed bytes, masking keys and TCP chunk boundaries are transport
encodings, not JSON protocol content. Socket close/control observations are also
retained. Client-initiated teardown is outside the observation window; late
unsolicited frames are observed for 300 ms after each scenario.

`normalization-rules.json` lists the field paths used by `normalize.mjs`:
owned tokens and scratch prefixes, generated context/message/input-epoch IDs,
specific timestamp/duration fields, and the validated random WebSocket accept
value. Identity maps preserve repeated references. Only the epoch inside
`inputState` is normalized; its presence, schema, counters and extra fields are
still compared. Numeric timing fields preserve null and nonnumeric values.
There is no blanket timestamp, path, PID, revision or unknown-field removal.
Ports, fingerprints, PIDs and source revisions are not presently exposed in the
compared frames; any future occurrence requires its own reviewed rule.

`allowed-differences.json` uses exact JSON Pointer paths and both exact values.
An absent field/frame is `{ "$missing": true }`. Large strings can be represented
by `{ "$string": { "length": N, "sha256": "..." } }`; the comparator checks both
and requires every surrounding object key to match exactly. An unused entry,
duplicate ID, mismatched allowance or unexplained difference fails the run.
Allowances are never automatically learned by the runner.

The report directory contains:

- `builds.json`, build logs on cache misses, and normalized complete transcripts
  as `CYCLE-SIDE-SCENARIO.json.gz` (including large payloads).
- `comparison.json`: every field difference, the allowance used, stale entries,
  and per-scenario frame counts.
- `report.txt`: per-scenario counts, difference/allowance summary and elapsed time.
- `failure.txt` and incomplete transcript/server logs if a scenario cannot finish.

The self-test starts from an actual v3.2.18 transcript excerpt. It seeds an extra
field, changed value, missing frame, reordered events and changed error code;
each must fail. It also checks stale/mismatched allowances, digest shapes,
transport boundaries and preservation of unknown normalized metadata.

## Extend

Add a named async function in `scenarios.mjs`. Use the owned server's client,
repository and paths. Use fixed request IDs, wait for causally relevant responses
or events, and assert key success conditions so identical setup failures cannot
masquerade as successful coverage. Run it against the same binary first, then
run the full three-repeat stability gate. Keep original receive order.

For a new volatile field, add the exact channel and field path to both the rules
list and normalization implementation. Explain why that value is generated and
preserve nulls, presence, identity relationships and all unknown siblings. Prefer
a deterministic fixture to a normalization rule. Never normalize a product
behavior difference, including extra metadata or error codes.

For an intended difference, inspect both transcripts and the source path, add an
exact allowlist entry with a durable reason, and run the full pair. Each entry
must be exercised. Removing or changing the underlying difference requires
removing or revising its allowance; a stale allowance cannot silently pass.

## Coverage and deliberate limits

Covered: gzip/plain auth, handshake headers, version/capabilities, wrong/missing
token, protocol mismatch, ping, unknown/missing channel, invalid JSON, wrong
parameter types, 64 MiB decompressed inbound limit, profile listing/open/status/
close/scoping, workspace save/load and second-client ordered broadcasts,
concurrent rejected saves, filesystem/Git operations, agent catalogs and unknown
session probes, one fake start/send per Claude and Codex runtime, Codex live
history attachment, retained PM start/resume/send rejection, a 5 MiB outbound
workspace event/result, 64-connection limit, and 128-concurrent-invoke limit.
The last uses owned FIFOs to block 128 readers, followed by a ping barrier and
a deterministic busy response; teardown terminates the blocked server threads.

Not covered in round 1:

- Successful simultaneous workspace saves: two independent mutation threads can
  interleave `profile:changed`, `workspace:reload` and acknowledgements. The
  exploratory run exposed that nondeterminism. We retain the sequential ordered
  mutation test and concurrent rejection test; we do not sort events to hide it.
- PTY execution/output: byte chunking, shell startup and exit ordering need a
  dedicated deterministic shell/PTY fixture. No terminal was launched here.
- `REMOTE_OUTBOUND_BURST` 1 vs 64 and the 30-second invoke/history queue-space
  wait: ordinary drain tests do not saturate the 256-frame queue. A paced native
  producer plus controlled slow-reader fixture is needed; content equality
  makes no claim about this back-pressure policy or wall-clock timing.
- Disk-backed Codex history paging and explicit `historyBefore` requests: the
  fake has no rollout file. The suite measures additive `before`/`hasMore` on
  live attachment, not paging correctness or large `claude:history` events.
- Live PM launch, receipt integrity, sandboxing and validation of
  `pmRuntimeId`/`pmContextDigest` on a live PM producer. The fields are sent on an
  ordinary fake Codex request, where neither host changes behavior. The actual
  PM path needs its dedicated offline sandbox fixture; no real provider runs.
- Disk error-log output, certificate pinning, unauthenticated handshake expiry,
  binary-frame abuse permutations, live-provider auth, mobile UI, desktop-only
  channels, remote-profile relays, reconnect/replay, macOS and Windows.

The closed mobile client is not available. This is a differential protocol gate
with explicit coverage, not a claim that every possible client is indistinguishable.

## Round 1 measured baseline (2026-11-13)

Upstream commit `07e80f56275075195d05d58cf67a208934874557` and fork runtime
revision `8fa0bb9` (also rebuilt from harness commit `a36ae7b`; no runtime-source
changes), both stamped `3.2.18`, produced these measured differences:

| Behavior | Upstream | Fork | Exact allowances |
| --- | --- | --- | ---: |
| `claude:get-pm-runtime` for an unknown session | Sidecar metrics and method-not-found | `PM_RUNTIME_RECEIPT_UNAVAILABLE` | 9 |
| Session metadata on fake Claude/Codex turns | No `inputState` | Native input epoch/schema/counters | 21 |
| Live Codex `client-resume` result | No cursor fields | `before: null`, `hasMore: false` | 2 |
| 5 MiB `workspace:reload` event | Delivers original event | `remote:payload-too-large` | 12 |
| Retained PM ownership directory | Ordinary fake start/resume; ordinary missing-session send error | `PM_RUNTIME_IMMUTABLE` on start/resume; `PM_RUNTIME_RESTART_FORBIDDEN` on send | 27 |

The 71 allowances include duplicate `params`/legacy `args` representations and
individual missing frames; they represent five behavior categories, not 71
independent features. Every allowance matched. All 21 scenarios passed with
547 upstream and 535 fork sent/received frames, zero unexplained differences and
zero stale entries. Both hosts returned the same `REMOTE_RESPONSE_TOO_LARGE`
invoke error for the large workspace load: that suspected difference was refuted
for this path. Extra PM parameters on the ordinary fake Codex send also did not
change the result; this does not exercise live PM validation.

Three consecutive full upstream-versus-upstream runs of the final harness
passed: 63 scenario comparisons, zero differences, 217.425 seconds total.
The 21 comparator/transport/normalization self-checks passed. Root TypeScript
checking and renderer compilation passed. Workflow YAML, trigger keys, all eight
full-SHA action references and six shell blocks were validated locally; GitHub
Actions itself and its dependency installation/checkout steps were not run here.

An end-to-end run from an empty scratch build cache took **293.575 seconds**;
its upstream and fork builds took 109.599 and 111.790 seconds. The second full run against the same cache took **72.272 seconds**,
with both build receipts reporting `cached: true` (no Cargo rebuild). These are local debug-build measurements,
not CI performance guarantees. No desktop build, full Rust unit suite, full
sidecar suite, macOS/Windows run, live server or real model was exercised.

Exploration initially found client envelope/length-parser mistakes and a fixture
import/preset mistake; those were fixed, not allowlisted. Time/ID/path fields were
then scoped explicitly, fixture diagnostic ordering was made deterministic, and
successful concurrent-save ordering was left uncovered as described above.
