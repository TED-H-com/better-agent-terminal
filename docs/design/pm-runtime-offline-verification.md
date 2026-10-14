# PM runtime offline verification

`.github/workflows/pm-runtime-offline.yml` checks two separate source revisions
on fresh GitHub-hosted Ubuntu 24.04 x64 VMs:

- Official upstream baseline: `b7419892fbc9946799b64cca24c2ec8c7fa15c42` in
  `tony1223/better-agent-terminal`.
- Feature: the exact PR head SHA, or the push SHA when no PR event is present.
  Later changes on the review branch therefore receive new verification.

The owner fork's recorded main is
`478c3ad0cf3f53c3da82e7df7179ad070003e635`. Its 44 upstream prerequisites are
separate from the four feature commits `04d76a7`, `a1ee88e`, `dc203ad`, and
`c397fd7`. This additional CI/documentation commit does not rewrite those commits
or update any main branch. The baseline job checks the prerequisite source; it
does not contain or claim to test the feature's PM confinement canary.

Pushes to `feat/pm-v12-native-input-identity-final` and PR updates run verification.
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
