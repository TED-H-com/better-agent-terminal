# PM runtime offline verification

`.github/workflows/pm-runtime-offline.yml` checks two separate source revisions
on fresh GitHub-hosted Ubuntu 24.04 x64 VMs:

- Official upstream baseline: the commit that the workflow's matrix pins in
  `tony1223/better-agent-terminal`. It is the upstream release the fork is based
  on, the commit recorded in `bac/UPSTREAM_BASE.json`; `bac-ci.yml` fails while
  the two differ, so a synchronization with a newer release moves both.
- Feature: the exact PR head SHA, or the push SHA when no PR event is present.
  Later changes on the review branch therefore receive new verification.

The baseline job checks upstream's unchanged source. It does not contain or claim
to test what the fork adds, for example the PM confinement canary.

When the workflow was added on 2026-10-11, the baseline was
`b7419892fbc9946799b64cca24c2ec8c7fa15c42` and the owner fork's recorded main was
`478c3ad0cf3f53c3da82e7df7179ad070003e635`. Its 44 upstream prerequisites were
separate from the four feature commits `04d76a7`, `a1ee88e`, `dc203ad`, and
`c397fd7`; the commit that added the workflow did not rewrite those commits or
update any main branch. The baseline moved to upstream v3.2.18 when the fork
was synchronized with that release.

Pushes to `main` and pull-request updates run verification. The workflow has no manual trigger:
code scanning reports one as a cache-poisoning risk for this job, which builds and runs a checkout
of the pinned upstream baseline.
Both events share a head-repository/head-branch concurrency group so a new run
cancels duplicate or stale work. The workflow has read-only repository access,
does not persist checkout credentials, and uses no repository secrets. It has no
tag trigger, release, deployment, BAT installation, or provider/model step.
Dependency downloads require network access; the tests use fake native servers,
temporary Git/filesystem resources and local sidecar transports.

## Checks and prerequisites

Node 24.21.0, pnpm 10.33.2 and Rust 1.98.1 are fixed to the local validation
toolchain. Actions are pinned to commit hashes. Both pnpm lockfiles are installed
with `--frozen-lockfile`; Cargo receives `--locked`. The Ubuntu dependencies are
`bubblewrap`, `build-essential`, `pkg-config`, `python3`, `git`, and
`ca-certificates`. No GTK/WebKit stack or desktop bundle preparation is needed.
The fake PM app-server is Python; these checks do not require a real Codex 0.160.0
installation, authentication, or a model request.

Each job runs the existing commands in order:

```sh
pnpm exec tsc --noEmit --pretty false
pnpm run compile
pnpm run test:sidecar
pnpm run check:tauri-rust --locked --no-default-features --features headless
pnpm run test:tauri-rust --locked --no-default-features \
  --features headless --no-fail-fast -- --test-threads=1
```

The Rust commands cover the headless workspace check and all workspace library
tests, following the existing scripts. Disabling the desktop feature is required
to include the Linux PM tests. Temporary data and Cargo outputs use separate disk
directories under `RUNNER_TEMP`; tmpfs/ramfs is rejected, and HOME is unchanged.
The checked-out source SHA and tool versions are recorded in the job output.

In the baseline job the last command also receives `--exact --skip` with the
name of one upstream test, after `--test-threads=1`. The section "One upstream
test is not run in the baseline job" gives the reason.

Ubuntu 24.04's enabled AppArmor user-namespace restriction needs a per-program
exception for bubblewrap's namespace setup. The job copies the distro binary to
the fresh, root-owned `/opt/bat-pm-ci/bin/bwrap`, checks its exact bytes, SHA256,
owner, mode and absolute path, and loads one named AppArmor profile attached only
to that path. The profile follows Ubuntu's per-application `userns` allowance;
`flags=(unconfined)` applies to that profile, not to AppArmor globally. Both the
global restriction (asserted as `1` before and after loading) and the system
`/usr/bin/bwrap` policy remain unchanged. Only this job's PATH receives the copy.
The profile is loaded into the disposable VM's kernel, with no persistent system
profile file or host-wide sysctl update. Ubuntu's approach is documented in
[restricted unprivileged user namespaces](https://ubuntu.com/blog/ubuntu-23-10-restricted-unprivileged-user-namespaces).

Before Rust checks, a real `inotify_init1` probe and a bounded bubblewrap PID/network
namespace probe must succeed using that exact copy; profile loading alone is not
acceptance. The feature's full Rust suite then runs the actual
read-only selected-checkout/receipt and owned-directory canary. Missing bubblewrap,
exhausted inotify resources, or AppArmor/user-namespace denial fails the job; there
is no skip, global policy relaxation, or root-run substitute. Any required runner
provisioning must be reviewed explicitly before changing this workflow.

## One upstream test is not run in the baseline job

`remote_server::tests::login_deadline_bounds_partial_reads_and_can_be_cleared_after_auth`
gives a stream a login deadline of 100 ms while a peer writes one byte every
20 ms, and first expects a read to end with `TimedOut`.

At upstream v3.2.18 the result of this test on Linux depends on timing in the
code under test. `DeadlineTcpStream::read` sets the socket's receive timeout to
the time that is left until the deadline and returns what the socket returns.
When that socket timeout expires before the next byte arrives, Linux reports
`EAGAIN`, which Rust returns as `WouldBlock`. `TimedOut` comes only from the
deadline check at the start of a later read. Whether the read ends with
`WouldBlock` or with `TimedOut` therefore depends on when the peer's next byte
arrives, that is, on scheduling.

On 2026-11-14 the first attempt of four baseline jobs failed this way, with
`left: WouldBlock` and `right: TimedOut`:
[43641881612](https://github.com/TED-H-com/better-agent-terminal/actions/runs/43641881612/attempts/1),
[43644676596](https://github.com/TED-H-com/better-agent-terminal/actions/runs/43644676596/attempts/1),
[43649735820](https://github.com/TED-H-com/better-agent-terminal/actions/runs/43649735820/attempts/1) and
[43653848935](https://github.com/TED-H-com/better-agent-terminal/actions/runs/43653848935/attempts/1).
The feature jobs of the same attempts passed, and each baseline job passed when
it was run again. A standalone copy of both reads, run 120 times each on a
Linux host with Rust 1.98.1, returned `WouldBlock` 15 times with upstream's
read and never with the fork's.

The fork's `DeadlineTcpStream::read` reads again when the socket times out
while a login deadline is set, so only the deadline check ends the read, with
`TimedOut`. The feature job therefore runs the test. The baseline job tests
upstream's unchanged source, where a failure of this test says nothing about
the fork, so that job leaves this one test out: the matrix entry
`skipped_rust_test` names it, and the test command receives
`--exact --skip <name>`. With `--exact` the name matches that one test only;
without it `--skip` would match every test whose name contains the text.

Three things keep the omission visible and bounded:

- Before the tests run, the job lists the tests of the checked-out source and
  fails unless the name is exactly one of them. A test that upstream renamed or
  removed cannot be skipped silently.
- The job summary names the test as not run, and the result line of its test
  binary counts it as `1 filtered out`.
- The entry belongs to the pinned baseline commit. When a synchronization moves
  the baseline, `bac/SYNC.md` asks for a new decision: the entry is removed
  once upstream's read no longer returns the socket's own timeout while a
  deadline is set.

No failure is forgiven and no test is run a second time: every other test of
the baseline job, and every test of the feature job, fails its job as before.

## Meaning of a result

The earlier `a1ee88e` full headless run recorded 556 passed, one failed and one
ignored. The unchanged filesystem watcher failed while independent inotify
initialization on that shared host returned `EMFILE`. At `c397fd7`, the focused
102 native tests passed, but that did not close the whole-workspace evidence gap.
A successful fresh feature job supplies new whole-workspace evidence for its
recorded SHA; the old failure remains part of the review history.

The first GitHub run at `ab69c2c`,
[38187107006](https://github.com/TED-H-com/better-agent-terminal/actions/runs/38187107006),
passed TypeScript, renderer build, sidecar and real inotify checks in both jobs.
Both then failed bubblewrap's loopback address setup with
`Failed RTM_NEWADDR: Operation not permitted` while the AppArmor restriction was
enabled. The per-binary CI provisioning addresses namespace capabilities needed
for that setup; only a subsequent passing real probe/canary can confirm it. Those
failed jobs did not run Rust, and remain failed evidence rather than being relabeled.

The existing environment-sensitive
`commands::claude::tests::supported_commands_native_reads_project_commands`
test remains ignored by upstream and must still be reported as ignored. CI does
not establish desktop packaging, Windows/macOS runtime behavior, production-host
namespace availability, authenticated model execution, installation, or Connector
end-to-end acceptance. No success is claimed until both matrix jobs complete.
