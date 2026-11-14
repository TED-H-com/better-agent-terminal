# Host identity handshake

Authentication adds only `capabilities.bac = {"protocol":"bac/1","minor":0}`
to version 2 results. Authentication does no identity I/O. Version 1 is unchanged.
`serverVersion` remains the upstream version. `workspaceRead` and `sessionLocate`
are true; `workspaceAddTerminal` and `gitObserve` are false.

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
preconditions and, for saved snapshots, requires exactly one saved window.

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
window IDs contribute their profile IDs. The append and git channels remain
unavailable.
