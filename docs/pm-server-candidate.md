# Fixed BAT PM server candidate

This Linux x86_64 package delivers the headless BAT producer for Connector's
`bat-pm-linux-v1` adapter and BAT's `codex-pm-linux-v1` runtime profile. It keeps
ordinary BAT sessions on their existing runtime. Connector still owns scopes,
managed resources, frozen context, original operation keys and adoption.

The **PM server candidate package** workflow builds the exact PR head (or the
selected manual workflow revision), using locked dependencies and pinned build
tools. Download its `bat-pm-server-linux-x86_64-<full-source-SHA>` artifact from
the matching successful run. It contains a source-named tarball, `PM-PACKAGE.json`,
`package-smoke.json` and `SHA256SUMS`. This workflow does not publish a release,
change the stable update channel or install on an existing host. CI artifacts
expire after 30 days; retain the verified download with the deployment record.

## Verify the candidate

Compare `PM-PACKAGE.json.source_revision` with the full reviewed commit and the
workflow checkout. In the downloaded directory, run `sha256sum -c SHA256SUMS`.
The manifest records each packaged file, its SHA256 or internal symlink, the
source repository, ordinary runtime versions and PM prerequisites. A checksum
detects changed bytes; it does not replace checking the trusted build source.
Committed product versions remain `0.0.1-dev`; **the full source SHA and binary
SHA256 identify this candidate**, not that development version string.

The package check extracts the actual archive into a disk directory, verifies
its contents, runs its BAT/Node/Codex binaries, syntax-checks its sidecar, starts
its BAT server on loopback with private disposable state, authenticates, rejects
a wrong token, and reads the PM endpoint. It restarts the server using the same
state and checks that certificate identity and token are preserved. A missing
PM receipt must remain `PM_RUNTIME_RECEIPT_UNAVAILABLE`, without creating a
session or ownership record. Only the test's process group and directory are
removed. This is packaged startup/readback evidence, not live model, installed
desktop, host-policy or project acceptance.

## PM prerequisites on the selected BAT host

- Linux x86_64 and glibc 2.28 or newer for bundled Node. The BAT binary itself
  uses static musl. Other producer platforms are not validated by this package.
- Bubblewrap on the service user's PATH, with the required namespaces usable
  under that host's existing policy. A host refusal is unsupported; do not
  disable administrator policies to turn a failed preflight into a pass.
- Exactly **Codex CLI 0.160.0**, corresponding to reviewed source
  `a956835d020762cb2b570053af06f643a11c0ecc`. Set `BAT_PM_CODEX_PATH` to that
  independently provisioned, trusted executable's absolute path. Verify its
  `--version` and retain its hash in the deployment record. This package does
  **not** include that PM executable. Its ordinary bundled Codex is 0.159.2;
  it does not satisfy PM preflight and should not be substituted for the PM path.
- The service user's existing Codex sign-in, with a regular readable
  `auth.json` in the existing Codex home. The service should run as that same
  non-root account. Preserve its HOME and any established CODEX_HOME setting;
  do not copy credentials into this package, its PM worktree, or a new profile.
  An auth file's presence does not prove a working model session.
- Existing administrator Codex configuration is respected: this fixed runtime
  refuses unsupported `/etc/codex` policy files. Do not move or delete them to
  bypass the refusal. See bundled `PM-RUNTIME.md` for the exact restrictions.
- Connector must have an explicit BAT host/workspace binding, a PM-owned
  worktree, current membership and the existing observe/manage/start scopes.
  Reading capabilities does not launch a model or attest runtime readiness.

## First installation

Extract into a new, versioned directory owned by the service user, for example
`~/.local/opt/bat-pm/<source-SHA>/`. Keep the directory unchanged after verifying
it. The generic overwrite-and-restart installer is intentionally absent from
this candidate. Run `bat-server --help` before activation.

Use a dedicated persistent data directory and an owner-only token file outside
the package directory. Set their directory permissions to 0700 and the token
file to 0600. For a new deployment, generate a random token without printing it;
for an upgrade, retain the existing token. Launch the binary directly, using
absolute paths and the actual deployment's port:

```sh
BAT_PM_CODEX_PATH=/absolute/path/to/codex-0.160.0 \
  /absolute/versioned/bundle/bat-server --bind=localhost --port=9876 \
  --token-file=/absolute/private/token --data-dir=/absolute/persistent/state
```

The server's current startup banner includes the token. Keep its console and
service logs private; do not attach them to issues or public CI artifacts.
Pair through the existing authenticated transport and verify the server's
certificate fingerprint through a trusted channel. Keep loopback binding when
using an existing tunnel; a broader network binding is a separate deployment
choice. For a supervised installation, use these same arguments, account,
working directory and explicit PM executable in the existing service manager.
Do not run a second server against the same state directory.

## Upgrade, readback and rollback

1. Record the existing binary/source hashes, service configuration, data path,
   token-file path and server fingerprint in the private deployment record.
   Stage and verify the new candidate in a separate versioned directory.
2. Pause new managed work through Connector's existing lifecycle controls.
   Reconcile already-sent operations using their **original** keys, session
   IDs, cursors and receipts. Do not infer completion from an idle session or
   an old worktree. Do not stop a server while another person's BAT session or
   an unresolved managed runtime still depends on it.
3. Once the selected host is quiescent, stop only its identified service and
   back up the persistent data, including encrypted credentials/certificate
   material and `pm-runtimes` receipts. Keep the backup private. Point the
   service at the new directory; preserve the same account, state, token,
   binding, port and PM executable configuration. Start the service once.
4. Verify the unchanged server identity and authenticate. Connector should
   read the original operation and `claude:get-pm-runtime {sessionId}` before
   deciding whether new work can proceed. A missing receipt, uncertain send
   or unproven process death remains unknown; restarting does not authorize a
   replacement launch, resend, lock clearing or deletion of runtime history.
5. If activation fails before admitting new work, stop that identified service
   and restore the previous executable configuration against its compatible
   retained state. If the new version has created state or sent effects, do
   not restore a stale backup over those records. Keep both versions and the
   new evidence for reconciliation; downgrade compatibility needs explicit
   validation. Never run old and new servers concurrently against that state.

After installation, record package hash, actual executable hash, PM CLI hash,
Connector version and the observed original runtime proof. Run one authorized
PM analysis in its own managed resources to establish live acceptance. Keep
this result separate from package CI and from ordinary BAT session health.
