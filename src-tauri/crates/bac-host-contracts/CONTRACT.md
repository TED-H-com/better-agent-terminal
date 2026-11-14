# Host identity handshake

Authentication adds only `capabilities.bac = {"protocol":"bac/1","minor":0}`
to version 2 results. Authentication does no identity I/O. Version 1 is unchanged.
`serverVersion` remains the upstream version. `workspaceRead`, `sessionLocate` and
`workspaceAddTerminal` are true; `gitObserve` is false.

`bac:hello` takes `{protocol:"bac/1", minor:0, expectedHostId?:string}` in the
existing invoke envelope. Validation order is shape, size, presence of `contextId`,
version, storage, expected identity. The 65,536-byte request limit measures compact
re-serialization of the decoded frame, not received bytes. Any `contextId` value,
including null, is refused. Results are bounded to 524,288 bytes including the
invoke-result envelope. Other BAC names require hello. Disabled features return
`UNSUPPORTED_FEATURE`; unknown channels return `UNKNOWN_CHANNEL`.

A refused hello retains an existing negotiation except `HOST_MISMATCH`, which
revokes it and returns the actual host ID. State transitions are serialized, not
ordered by frame arrival. Wait for the hello reply before sending another BAC
request. Storage refusals are not cached: each hello retries filesystem work up
to the refusal. Successful initialization is cached for the process.

## Identity storage

Only `<data-dir>/bac/host.json` is persistent contract state. Its exact content is
`{"schema":1,"hostId":"h_<32 lowercase hexadecimal digits>"}\n` (the same two keys
in sorted order are also accepted), at most 256 bytes. IDs use OS randomness.
Copies preserve identity. Missing data roots are never created. The configured
data root can be a symbolic link or junction; it must resolve to a directory.

Unix opens that root once and performs bac creation/open relative to it. The
opened bac directory must belong to the effective user and have no group/other
write bits. Every identity/temp open, link, unlink and cleanup check is relative
to that verified bac handle. Identity metadata comes from the opened nonblocking,
no-follow file handle; only a regular file with the same ownership/write-bit
requirements is accepted. Creation is 0700/0600; existing permissions are never
changed. Before returning or caching, the root's bac entry must still identify
the verified directory by device/inode, otherwise initialization is unavailable.
An account able to replace data-root entries can lose/change identity as it can
replace the host certificate/token. It cannot redirect this store's writes outside
a verified directory or make it adopt an identity from an unchecked entry.
These checks protect against other users and link redirection, not another
process of the same user: it can replace the final entry with another valid private
identity. Cleanup removes old temporary entries by name within the verified
directory; it does not prove that the removed inode was the inspected one.
Windows instead relies on the data root's ACL; bac and host.json reparse points
are refused. Reading shares read/delete, permitting unlink of a publisher's temp
hard link without changing the opened file or permitting concurrent writes.

Publication creates a new private temporary file, writes and syncs complete
contents, then hard-links it to host.json without replacement. An existing target
wins and is read back. Real pre-publication I/O errors remove the candidate best
effort; crash injection leaves it. After publication, temporary removal is best
effort and does not change the result. Temporary files are never read as identity;
matching regular, owned files older than 24 hours are eligible for cleanup.
Directory handles are synced on Unix. Unsupported directory sync (EINVAL,
ENOTSUP/EOPNOTSUPP, EBADF) is tolerated; real I/O errors refuse. Windows standard
library cannot sync directories. On these filesystems/platforms a power loss can
require a new identity, detected through expectedHostId by Connector.
Temporary-file `File::sync_all` is strict (full sync on Apple), so a filesystem
without that support refuses creation with `STORAGE_UNAVAILABLE` and
`details.reason: "temp_sync"`; existing identities remain readable.
Only unsupported directory sync is tolerated as described above.

## Refusal reasons and diagnostics

For unchanged storage situations, storage errors retain their codes/messages and
add only a reason. Invalid entry types (including sockets) are `STORAGE_CORRUPT`;
other failed opens are `STORAGE_UNAVAILABLE`. After a failed Unix open, a no-follow
inspection relative to its parent handle determines the entry type; a failed
inspection also means unavailable. These replies return only
`details: {"reason":"<fixed name>"}`. New names may appear: clients treat unknown
names as unspecified. No path, contents or host ID appear in these details.
The operator receives a fixed reason and redacted path through stderr everywhere
and through the error spool only where the host installed it, at most once per
reason per process. Desktop has no installed spool and receives stderr only. This diagnostic never opens a file under the
data directory, regardless of BAT_DEBUG. The host's other diagnostics are separate.

Client-visible reason names:

```text
data_directory_unavailable
data_directory_type
directory_metadata
directory_type
directory_open
directory_integrity
directory_read_only
directory_create
directory_sync
identity_metadata
identity_type
identity_integrity
identity_size
identity_contents
identity_changed
identity_open
identity_read
random
temp_create
temp_write
temp_sync
publish
```

`cache_poisoned` exists only in the host diagnostic for an `INTERNAL_ERROR`;
that reply has empty `details` and exposes no reason.

## Build and compatibility limits

Release builds must set BAC_BUILD_REVISION (40 lowercase hex digits),
BAC_BUILD_RELEASE (printable ASCII up to 64 bytes) and BAC_BUILD_DIRTY (true/false).
Defaults are unknown/dev/null; the crate never runs Git at build time.

The fork currently caps outbound frames at 4,194,304 bytes, including authentication
results; upstream v3.2.18 does not cap that frame. The constant advertisement adds
37 serialized bytes. A baseline result of 4,194,303 bytes therefore becomes a
REMOTE_RESPONSE_TOO_LARGE error. This known fork interaction must disappear when
the cap is removed from authentication; no cap change is part of this contract.


## Read-only workspace and session location

`bac:workspace-read` accepts exactly `{ "profileId": <ProfileId> }` and
`bac:session-locate` accepts exactly `{ "sessionId": <SessionId> }`. A
`SessionId` is 1–128 UTF-8 bytes, each in `[A-Za-z0-9_.:-]`. A `ProfileId` is
1–128 UTF-8 bytes, each character ASCII alphanumeric, `_`, `.`, `-`, or in
`U+4E00`–`U+9FFF` inclusive (`:` is not allowed). This grammar applies to
request fields and catalog addressability only; stored workspace, terminal, and
window IDs are not rewritten or rejected by workspace read. Invalid input returns
`INVALID_REQUEST` with a static `details.field`, without echoing caller values.
Feature operations use the host ID negotiated by `bac:hello` and release the
negotiation lock before reading application state.

Read returns `hostId`, `profileId`, `source` (`live` or `persisted`), nullable
`windowId`, `revision`, `document`, and `appendTargetEligible`. A local profile
uses the latest nonempty regular in-memory window; detached windows are never the
read target. Otherwise it uses the first saved window; no saved windows produce
the five-field empty workspace with `appendTargetEligible: false`. The document
is not overlaid with runtime metadata. `appendTargetEligible` reports structural
preconditions and, for saved snapshots, requires exactly one saved window. For
saved snapshots, `appendTargetEligible` also requires the raw first window itself
to contain `workspaces` and `terminals` arrays of objects with non-empty string
`id` fields, even when the rendered `document` defaults missing fields to `[]`.

```json
{"hostId":"h_0123456789abcdef0123456789abcdef","profileId":"default","source":"persisted","windowId":null,"revision":"sha256:66c1b7a04a05ee220e10341c9baec59bdf92885b193c2838d527a235d2ca5a9c","document":{"workspaces":[],"activeWorkspaceId":null,"activeGroup":null,"terminals":[],"activeTerminalId":null},"appendTargetEligible":false}
```

A document revision is `sha256:` followed by the lowercase SHA-256 of compact
UTF-8 JSON with object keys recursively sorted and array order retained. `null`
and absent fields remain distinct. The empty example hashes the canonical
100-byte serialization, not the empty byte string.

Locate observes the in-memory registry, then local catalog profiles and saved
profile windows sequentially; it does not promise an atomic cross-source view.
It returns `hostId`, `locationRevision`, `disposition` (`unregistered`,
`located`, or `ambiguous`), `consistency: "sequential"`, `atomic: false`, and
sorted `locations`. Each row contains `profileId`, `workspaceId`, `terminalId`,
nullable `windowId`, `source`, and `profileRevision`. A regular live placement
suppresses a matching saved placement; a detached window contributes only its
own workspace and suppresses no saved placement. Exact five-tuples are
deduplicated. A row's `profileRevision` is the revision workspace read would
return for that profile from the same observation, not a digest of the window
where that row was found. `locationRevision` hashes the sorted multiset of
profile, source, window, and document revision tuples across all observed local
documents.

Before the desktop host loads its registry, `windows.json` is not consulted;
read and locate use saved profiles. An empty in-memory registry means no live
placement, including in desktop-compiled servers. Neither operation seeds,
repairs, or hydrates the catalog or registry. Under the profile index lock, local
catalog reads inspect both `index.json` and `index.json.bak` with
`symlink_metadata` before reading; each must be absent or a regular file of at
most 8 MiB (with at least one present), otherwise the operation returns
`WORKSPACE_UNAVAILABLE` (or `LOCATION_UNAVAILABLE` with empty `profileIds` for
locate) without reading either catalog file. Remote and uncatalogued registry
entries are ignored. Saved profile reads inspect `metadata` (following symlinks) before opening,
open with `O_NONBLOCK` on Unix, and require `is_file()` on the opened handle
before reading. On Windows, any saved profile whose ID prefix before the first
`.` equals (ASCII case-insensitive) `CON`, `PRN`, `AUX`, `NUL`, `COM0`–`COM9`,
or `LPT0`–`LPT9` is treated as unreadable with 0 bytes read without touching the
filesystem. Missing paths and dangling symlinks produce no saved windows
with 0 bytes read; non-regular entries and metadata or open failures report
unreadability with 0 bytes read without retrying; regular files whose metadata
exceeds 8 MiB immediately report too large with 0 bytes read without retrying.
Readable regular files use a single parsed buffer, at most 8 MiB per file, with
up to three attempts for torn or invalid JSON and 20 ms between attempts. A
locate scan reads no more than 64 MiB in total.

The 11-key `bac:hello` limits map is unchanged: `requestBytes: 65536`,
`resultBytes: 524288`, `registrationBytes: 16384`, `profiles: 256`,
`terminals: 4096`, `locationMatches: 100`, `gitStatusBytes: 4194304`,
`gitIndexBytes: 16777216`, `gitPaths: 10000`, `gitTimeoutMs: 5000`, and
`gitAttempts: 2`. Compact canonical workspace documents and complete compact
locate results are limited to 520,192 bytes. A registry `windowId` is limited
to 1,024 UTF-8 bytes in JSON encoding, including quotes and escapes. If any
regular or detached window of the target local profile exceeds that bound,
read returns `WORKSPACE_UNAVAILABLE` without saved fallback; locate lists that
profile in `LOCATION_UNAVAILABLE.details.profileIds`. Locate also limits local
profiles to 256, observed terminals to 4,096 before filtering, and matching rows
to 100. These bounds keep the complete replies within the 524,288-byte frame
limit.

Operation failures have `{ "ok": false, "error": { "code", "message",
"retryable": false, "details" } }`:

| Code | Message | Details |
|---|---|---|
| `INVALID_REQUEST` | `Invalid BAC request` | `{ "field": <static path> }` |
| `PROFILE_NOT_FOUND` | `Local profile not found` | `{}` |
| `PROFILE_NOT_LOCAL` | `BAC requires a local profile` | `{}` |
| `WORKSPACE_UNAVAILABLE` | `Canonical workspace is unavailable` | `{}` |
| `WORKSPACE_TOO_LARGE` | `Canonical workspace exceeds limit` | `{ "maxBytes": 520192 }` |
| `LOCATION_UNAVAILABLE` | `Session locations are unavailable` | `{ "profileIds": <sorted unique IDs> }` |
| `LOCATION_TOO_LARGE` | `Session locations exceed limit` | `{}` |
| `INTERNAL_ERROR` | `BAC request failed` | `{}` |

A catalog failure or invalid local catalog ID returns `LOCATION_UNAVAILABLE`
with an empty `profileIds` list. Unreadable local saved documents or oversized
window IDs contribute their profile IDs. The git channel remains unavailable.

## Workspace terminal append

`bac:workspace-add-terminal` accepts
`{ "profileId": <ProfileId>, "expectedRevision": <Revision>, "terminal": <Registration> }`
with no additional keys. `profileId` uses the `ProfileId` grammar
(`valid_profile_id`), and `expectedRevision` must be `sha256:` followed by 64
lowercase hexadecimal ASCII digits (`valid_revision`). `terminal` must be a JSON
object whose compact UTF-8 encoding is at most `REGISTRATION_BYTES` (`16,384`
bytes) and whose keys are drawn only from `id`, `workspaceId`, `title`, `type`,
`cwd`, `agentPreset`, `model`, `permissionMode`, `agentParams`, `worktreePath`,
and `worktreeBranch`:

- `id` and `workspaceId`: `Id` grammar (`valid_id`, 1–128 ASCII bytes in `[A-Za-z0-9_.:-]`).
- `title` and `cwd`: non-empty UTF-8 strings without NUL (`\0`).
- `type`: `"terminal"`.
- `agentPreset`: `"claude-code"`, `"claude-code-worktree"`, `"codex-agent"`, or `"codex-agent-worktree"`.
- `model`, `permissionMode`, and `worktreePath`: optional; when present and non-`null`, each must be a non-empty string without NUL.
- `agentParams`: optional; when present and non-`null`, a JSON object of at most `AGENT_PARAMS_LIMIT` (`32`, in `bac_host_contracts::workspace`) entries with non-empty NUL-free string keys and NUL-free string values.
- `worktreeBranch`: optional; may be omitted, `null`, or a non-empty string without NUL.

Invalid parameters return `INVALID_REQUEST` with a static `details.field`
(`params`, `params.profileId`, `params.expectedRevision`, `params.terminal`, or
`params.terminal.<field>`).

`normalize_registration` produces the terminal JSON object written into the
workspace document: it writes the six required keys, omits `model`,
`permissionMode`, `agentParams`, and `worktreePath` when absent or `null`, omits
`worktreeBranch` when absent on the wire, and preserves an explicit
`"worktreeBranch": null` or string value. This omission rule applies only to the
registration object inside the stored workspace document, never to BAC wire
envelopes. Existing top-level, workspace, and terminal foreign keys in the
document are preserved.

`prepare_append` evaluates the target document in order:

1. If `windowId` exceeds `WINDOW_ID_JSON_BYTES` (`1,024` compact JSON bytes
   including quotes) or the current document fails `structural_preconditions_ok`,
   refuse with `WORKSPACE_UNAVAILABLE`.
2. Compute `(current_revision, current_bytes)` via `canonical_digest`.
3. If `terminals` already contains an entry with `terminal.id`, compare the
   stored terminal and normalized candidate with `effective_registration_equal`
   after applying the runtime overlay to both (defaulting missing `type` to
   `"terminal"`, treating absent and `null` as equal for `model`,
   `permissionMode`, and `worktreePath`, distinguishing absent, `null`, and
   string for `worktreeBranch`, and treating absent, `null`, and `{}` as equal
   for `agentParams`):
   - If effectively equal and `current_bytes > DOCUMENT_MAX_BYTES` (`520,192`),
     refuse with `WORKSPACE_TOO_LARGE` (`{ "maxBytes": 520192 }`).
   - If effectively equal and within `DOCUMENT_MAX_BYTES`, return an idempotent
     no-op `AppendResult` (`hostId`, `profileId`, `source`, `windowId`,
     `terminalId`, `appended: false`, `revision: current_revision`) even if
     `expectedRevision` does not match or `workspaceId` is absent.
   - Otherwise refuse with `TERMINAL_EXISTS`.
4. If `expectedRevision != current_revision`, refuse with `REVISION_MISMATCH`.
5. If `terminal.workspaceId` does not match any entry in `workspaces`, refuse
   with `WORKSPACE_NOT_FOUND`.
6. Append the normalized registration to `terminals`, check `canonical_digest`
   byte length against `DOCUMENT_MAX_BYTES` (`520,192`) before the runtime
   overlay, apply the runtime overlay, and check `canonical_digest` byte length
   against `DOCUMENT_MAX_BYTES` again after the overlay. If either check exceeds
   `DOCUMENT_MAX_BYTES`, refuse with `WORKSPACE_TOO_LARGE`. `prepare_append`
   returns the prospective document after the runtime overlay.

`verify_readback` confirms that a readback document satisfies
`structural_preconditions_ok`, matches the overlaid prospective document for
`terminal.id` under `effective_registration_equal`, and has canonical size at
most `DOCUMENT_MAX_BYTES`, returning `WRITE_FAILED` otherwise.

Append bounds reuse the existing constants (`REQUEST_BYTES = 65_536`,
`RESULT_BYTES = 524_288`, `REGISTRATION_BYTES = 16_384`,
`PROFILES_LIMIT = 256`, `TERMINALS_LIMIT = 4_096`,
`LOCATION_MATCHES_LIMIT = 100`, `DOCUMENT_MAX_BYTES = 520_192`,
`WINDOW_ID_JSON_BYTES = 1_024`, `SAVED_FILE_MAX_BYTES = 8_388_608`,
`CATALOG_FILE_MAX_BYTES = 8_388_608`, `SAVED_TOTAL_MAX_BYTES = 67_108_864`) plus
`AGENT_PARAMS_LIMIT = 32`. The largest live `bac:workspace-add-terminal` result
frame (`770`-byte `id`, `1,024`-byte JSON `windowId`, `130`-byte `profileId` and
`terminalId`, `appended: false`) serializes to `2,318` bytes, within
`RESULT_BYTES` (`524,288`).

Append-specific operation errors:

| Code | Message | Retryable | Details |
|---|---|---|---|
| `WORKSPACE_NOT_FOUND` | `Target workspace not found` | `false` | `{ "workspaceId": <workspaceId> }` |
| `TERMINAL_EXISTS` | `Terminal ID has a different registration` | `false` | `{ "terminalId": <terminalId>, "currentRevision": <revision> }` |
| `REVISION_MISMATCH` | `Workspace revision changed` | `true` | `{ "currentRevision": <revision> }` |
| `WRITE_FAILED` | `Canonical workspace save failed; read back before retry` | `false` | `{ "outcome": "unknown" }` |

For a saved profile (`source: "persisted"`, `windowId: null`), `append_persisted`
executes inside a single profile-index transaction (`update_index_at`) with the
following guarantees:

- Reserved Windows profile names (`is_reserved_profile_id`) are refused with
  `WORKSPACE_UNAVAILABLE` before opening any file.
- Inside the index lock, `catalog_files_acceptable` rechecks that `index.json`
  and `index.json.bak` are each absent or a regular file of at most
  `CATALOG_FILE_MAX_BYTES` (`8,388,608` bytes) with at least one present, and
  `require_local_profile` checks the first catalog entry matching `profileId`
  (`PROFILE_NOT_FOUND` or `PROFILE_NOT_LOCAL`).
- The profile snapshot is read with `read_bounded_saved_file` and
  `migrate_snapshot`. `NoSavedWindows` (missing file, dangling symlink, or
  `"windows": []`) evaluates against `empty_workspace_document()` without
  seeding or creating a profile file (stopping at `WORKSPACE_NOT_FOUND` when
  `expectedRevision` matches the empty workspace revision).
- Saved-profile append eligibility shares `saved_windows_append_eligible` with
  `bac:workspace-read`: only a single-window snapshot whose raw `windows[0]` and
  extracted workspace document satisfy the read eligibility check is accepted;
  multi-window or otherwise ineligible snapshots return `WORKSPACE_UNAVAILABLE`
  with the profile file untouched.
- After `prepare_append` produces the overlaid prospective document (or aborts
  without writing on an idempotent no-op or refusal), the snapshot is written
  through upstream's profile writer (`snapshot_from_workspace` and
  `write_snapshot_at`), re-read with `read_bounded_saved_file`, verified with
  `verify_readback`, and activated in the profile index
  (`activate_profile_in_index`) before the index commit.
- Stage and unwind rule: the index closure runs inside `catch_unwind`. Before
  the snapshot write starts, any I/O error or structural refusal returns
  `WORKSPACE_UNAVAILABLE` (or the specific refusal), and a panic returns
  `INTERNAL_ERROR`, without writing. Once the snapshot write starts (snapshot
  write, readback verification, index commit, or a panic at or after that
  point), any failure returns `WRITE_FAILED` with `{ "outcome": "unknown" }`.

For a live window (`source: "live"`, `windowId: "<id>"`), `append` /
`append_state` executes against the in-memory `WindowRegistryState` without
loading, repairing, or seeding the registry (`ensure_entries_ready` is not
called before target selection) with the following guarantees:

- Target selection shares `live_target` with `bac:workspace-read`
  (`execute_read`): among the profile's regular (`detached_workspace_id` is
  `None`) registry entries that have content (`snapshot_has_content`), the entry
  with the highest `last_active_at` is selected (`select_regular_entry`).
- If any registry entry of `profileId` (regular or detached) has a `windowId`
  whose compact JSON encoding exceeds `WINDOW_ID_JSON_BYTES` (`1,024` bytes
  including quotes), `live_target` refuses with `WORKSPACE_UNAVAILABLE` without
  falling back to the saved profile.
- When `live_target` returns `None` because the profile has no regular window
  with content (detached-only, contentless-only, or an empty registry),
  `entries` is unlocked and the append delegates to the saved-profile path while
  `runtime_meta` stays locked, passing `apply_runtime_workspace_meta` for
  `profileId` as the runtime overlay.
- The live write goes through upstream's own save path
  (`save_workspace_locked`), so the files (`windows.json`, `workspace.json` when
  `windowId == "main"`, and `profiles/<profileId>.json`) and bytes written are
  the same writer and the same bytes as upstream's `workspace:save` for the same
  document.
- Upstream's snapshot writers (`persist_entries_at`,
  `write_global_workspace_at`, `write_profile_snapshot_at`) do not report disk
  write errors; after `save_workspace_locked` returns `true`, the updated
  registry entry is read back via `workspace_value_from_snapshot` and checked
  with `verify_readback`, so the reply revision and staged document reflect the
  registry readback.
- `prepare_append` applies `apply_runtime_workspace_meta` to produce the
  prospective document and `save_workspace_locked` applies
  `apply_runtime_workspace_meta` before storing the snapshot, so the runtime
  overlay is part of the written document.
- Lock, stage, and unwind rule: `runtime_meta` and `entries` are locked in that
  order (a poisoned mutex returns `INTERNAL_ERROR` without recovery). Target
  selection and `prepare_append` run inside `catch_unwind`; before the write
  starts, any refusal returns its specific error and any panic returns
  `INTERNAL_ERROR` without calling the writer or poisoning the registry mutexes.
  From the write onward (including a writer return of `false`, a missing entry
  or mismatch on readback, or any panic during the write or readback), any
  failure returns `WRITE_FAILED` with `{ "outcome": "unknown" }` while leaving
  the registry mutexes unpoisoned. The saved-profile fallback returns the
  saved-profile path's own result (its refusals, its no-op, and its
  `WRITE_FAILED`), and only a panic that escapes it is `WRITE_FAILED` with
  `{ "outcome": "unknown" }` while leaving the registry mutexes unpoisoned.
- `live_window_ids_for_profile` / `live_window_ids_state` collects live webview
  window IDs before locking `entries`, returns the profile's regular live window
  IDs in registry order, skips detached and closed windows, and returns an empty
  list if `entries` is poisoned.

Dispatch and events (`dispatch_append`, `append_events`, `emit_append_events`)
enforce the following guarantees:

- Order of checks: `dispatch` routes `bac:workspace-add-terminal` to
  `dispatch_append` before any registry observation. A reserved Windows profile
  name (`is_reserved_profile_id`) is refused with `WORKSPACE_UNAVAILABLE` before
  any file is opened; an unresolved host data directory or a missing or
  unacceptable catalog (`read_local_catalog` / `catalog_files_acceptable`)
  returns `WORKSPACE_UNAVAILABLE`; then `require_local_profile` resolves the
  first catalog entry of `profileId` (`PROFILE_NOT_FOUND` or
  `PROFILE_NOT_LOCAL`). The append takes no separate registry snapshot: on
  desktop builds, the choice between the live window and the saved profile is
  made under the registry locks (`runtime_meta` then `entries`); on headless
  builds, the append delegates directly to `append_persisted`.
- Events: emitted only after every registry and profile-index lock is released,
  inside `catch_unwind`; an error or panic while sending never changes the
  reply, and a no-op (`appended: false`) or a refusal sends nothing. A
  live-window append sends `workspace:reload` with
  `{ "profileId", "windowId", "data" }` to that window (`emit_to`) and the same
  payload to remote clients (`broadcast_event`), and no `profile:changed`
  (matching upstream's `workspace:save` with a window ID). A saved-profile
  append sends `profile:changed` (upstream's payload), then `workspace:reload`
  with `{ "windowId", "data" }` to each open regular window of the profile
  (`live_window_ids_for_profile`) and `{ "profileId", "data" }` to remote
  clients (matching upstream's `workspace:save` without a window ID). In every
  `workspace:reload` event, `data` is the pretty-printed document read back
  inside the locked write (`staged.document`), never read again after the locks
  are released.
- Feature gate: `WORKSPACE_ADD_TERMINAL_ENABLED` is `true`, and `bac:hello`
  advertises `workspaceAddTerminal: true`. Like the other enabled features, the
  channel answers `NOT_OPTED_IN` before hello; after hello its params are parsed
  first, and a request that does not parse answers `INVALID_REQUEST` without
  running the append.
