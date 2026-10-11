# Dedicated native PM runtime

The additive `codex-pm-linux-v1` profile runs one fresh PM analysis turn through
BAT's existing Codex session, native event mapping and host replay. Connector
remains responsible for original operation authority, source visibility, fixed
context, managed resource ownership and original send-cursor persistence.
A context digest is correlation, never an authority grant.

This patch is based on audited upstream revision
`b7419892fbc9946799b64cca24c2ec8c7fa15c42`. That revision is 44 commits after the
owner fork's `478c3ad0cf3f53c3da82e7df7179ad070003e635`; its native per-session
runtime map and `claude:sync-session` protocol are prerequisites. This patch does
not update the owner fork main, install BAT, or modify the ordinary CLI catalog.

## Request and readback

Use existing `claude:start-session` with a new, never-used `sessionId` and:

```json
{
  "agentPreset": "codex-agent-worktree",
  "cwd": "/selected/logical/workspace",
  "codexSandboxMode": "read-only",
  "codexApprovalPolicy": "never",
  "pmRuntimeProfile": "codex-pm-linux-v1",
  "pmContextDigest": "<64 lowercase hexadecimal characters>",
  "pmTimeoutMs": 300000
}
```

Existing logical carrier fields `workspaceId`, `workspaceName`, `profileId`,
`useWorktree`, `worktreePath`, `worktreeBranch`, `model`, `effort`, and optional
`agentType: codex` remain accepted. They grant no native environment access. No
inline prompt, resume/reload, arbitrary native options, tools, configuration,
provider fallback or second turn is accepted. The selected BAT worktree remains
the session's logical source; the actual native cwd is a fresh private empty
directory. The total deadline begins at profile creation, including preflight,
and cannot be reset at send. Default is five minutes, maximum one hour.

Before the sole send, Connector persists the original `claude:sync-session`
epoch/sequence. Existing `claude:send-message` adds these PM fields:

```json
{
  "sessionId": "<original managed session>",
  "prompt": "<exact frozen text>",
  "images": [],
  "clientMessageId": "<original bounded client ID>",
  "pmRuntimeId": "<server-owned runtime ID from original start>",
  "pmContextDigest": "<same original context digest>"
}
```

The exact prompt, including whitespace, is preserved. The send intent with SHA256,
client ID, native request ID and consumed one-turn slot is fsynced before writing
`turn/start`. `clientMessageId` becomes native `clientUserMessageId`. Original
native user echo must match both that ID and the exact input digest. A lost ACK,
thread-not-found response, timeout or broken pipe cannot recreate a thread or
resend. Ordinary Codex send behavior is unchanged.

The final native spawn/registration and every PM stdin frame share a short stop
serialization gate. Stop requests cancellation before waiting for that gate;
accepted stop means no later frame or launch can begin. PM stdin is nonblocking,
and a bounded write loop checks cancellation and the original deadline even when
the native process stops reading. The gate is released before waiting for any
response. Partial frames retain the fsynced consumed send slot and remain unknown
across restart; they cannot become proof of an unsent operation or permit retry.

Read `claude:get-pm-runtime {sessionId}`. It returns `{sessionId, pmRuntime}`;
start results and live `get-session-meta.pmRuntime` contain the same record.
Ordinary replay metadata excludes this full proof record to avoid duplicating native
output inside bounded replay events. The dedicated readback retains the full proof.
The record includes profile/version, server-generated runtime ID, correlation
digest, BAT version/source/binary digest, pinned Codex version/source/binary
digest, original process identity (`pid`, Linux `startTicks`, `bootIdDigest`),
logical and runtime cwd, thread ID, actual model, effective config digest and
attestation. Optional fields are null until observed, never guessed.

Attestation reports confirmed effective restrictions, isolated config, no skills
or MCP servers, fresh thread and zero initial turns. It explicitly reports
`toolCatalog: not_exposed_by_protocol` and
`internalToolOptionsReadback: not_exposed_by_protocol`; pinned source-backed
restrictions are not an observed empty complete tool catalog. Auth-file readability
does not prove authentication or model execution.

The `send` record retains the exact attempted request and later native ACK,
turn-start and echoed client-ID observations. `terminal` is written only from an
original native `turn/completed`, preserving its full `event`, canonical
`eventDigest`, native thread/turn/status and, for completed output, native message
ID and output digest. A completed summary must equal the original completed final
assistant item, with matched item lifecycle and exactly one original user echo.
No streamed fragment, ACK, idle metadata, synthetic abort or process exit can
produce this terminal proof.

After the original native terminal is recorded and passed through the unchanged
BAT event adapter, the dedicated process group is killed/reaped. `exit` retains
actual exit code/signal, action, original identity and `confirmedDead`. Explicit
stop/abort, deadline and malformed/unexpected native output also stop only that
owned process, without emitting a synthetic `claude:turn-end`. Native completion
and confirmed death are independent requirements for successful Connector import.

## Isolation and retained evidence

Linux, bubblewrap and exactly `codex-cli 0.160.0` are required, with reviewed
source `a956835d020762cb2b570053af06f643a11c0ecc`. The dedicated path resolves
`BAT_PM_CODEX_PATH` (host-admin configuration), or `codex` on PATH, without
changing ordinary session resolution. Other versions fail closed. This does not
install or upgrade a runtime. The process uses a private PID namespace and owned
process group, a read-only root, a new profile and empty cwd; only profile/native
state and that empty directory are writable. Receipts and selected checkouts
remain read-only inside the namespace.

An existing regular auth file is opened read-only with `O_NOFOLLOW` and mounted
through its descriptor. No credential bytes are copied or logged. The process
receives a narrow environment allowlist; inherited API keys and runtime flags
are excluded. User configuration/skills are hidden in the child namespace; host
HOME/CODEX_HOME are preserved. Existing administrator Codex policy files cause a
preflight refusal rather than being hidden or weakened. Effective config,
skills, fresh thread sandbox/model/environments/instructions and MCP inventory
are validated before any model input.

The journal/receipt lives under the BAT data directory, keyed by a digest of the
exact original session ID. It is private and fsynced. The namespace cannot write
these records. Stop retains runtime/process/thread/send/terminal identities;
readback after BAT restart loads the original record and performs only a read-only
original-process identity observation. A missing or unreadable record is unknown,
never proof of no launch or death. Reusing that session ID is rejected; recovery
cannot adopt a different process or native thread. No automatic blanket cleanup
of journals or unrelated resources occurs.

Native transport limits are one MiB per JSON line, eight MiB stdout, 64 KiB
stderr, 256 item records and 128,000 output characters. Prompt encoding must fit
the same line boundary. At most 16 dedicated PM processes may be active in one
BAT instance. Protocol failure, tool item, server request, foreign identity,
incomplete item lifecycle or oversized output fails closed.

Offline tests use a fake app-server and actual temporary disk directories. A
separate bubblewrap canary checks real read-only selected-checkout/receipt mounts
and owned-directory writes without credentials, network or a model call. Headless
Rust, existing sidecar, TypeScript and renderer build checks are local evidence;
no live model, remote host, installed release or full product acceptance is
claimed by those checks.
