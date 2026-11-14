# Host identity handshake

Authentication adds only `capabilities.bac = {"protocol":"bac/1","minor":0}`
to version 2 results. Authentication does no identity I/O. Version 1 is unchanged.
`serverVersion` remains the upstream version. All four feature flags are false.

`bac:hello` takes `{protocol:"bac/1", minor:0, expectedHostId?:string}` in the
existing invoke envelope. Validation order is shape, size, presence of `contextId`,
version, storage, expected identity. The 65,536-byte request limit measures compact
re-serialization of the decoded frame, not received bytes. Any `contextId` value,
including null, is refused. Results are bounded to 524,288 bytes including the
invoke-result envelope. Other BAC names require hello, then return
`UNSUPPORTED_FEATURE` for the four advertised features or `UNKNOWN_CHANNEL`.

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
