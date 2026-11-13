# Merge an upstream release

Use a dedicated sync branch and retain review evidence outside the repository. Tests must use disposable homes, data directories and loopback listeners, never live sessions or real provider turns. Commands below assume Bash, Node 24, the pinned pnpm and the Rust toolchain used by CI.

## Procedure

1. **Record the starting revisions and fetch the release tag.** Start from a clean worktree. Replace `vX.Y.Z` with the selected release; do not merge a moving upstream branch.

   ```sh
   test -z "$(git status --porcelain)"
   old_fork=$(git rev-parse HEAD)
   old_base=$(node -p "require('./bac/UPSTREAM_BASE.json').commit")
   sync_tag=vX.Y.Z
   sync_review=$(mktemp -d)
   git fetch upstream tag "$sync_tag"
   new_upstream=$(git rev-parse "${sync_tag}^{commit}")
   printf '%s\n' "$old_base" "$old_fork" "$new_upstream" > "$sync_review/revisions.txt"
   git merge --no-ff --no-commit "$new_upstream"
   ```

   Keep a real merge commit; never rebase or cherry-pick the upstream release. Read the inventory plans and open/backported floor entries before resolving conflicts.

2. **Resolve conflicts and review automatic merges.** Capture the unresolved paths and inspect both changes from the old base:

   ```sh
   git diff --name-only --diff-filter=U > "$sync_review/conflicts.txt"
   git diff "$old_base" "$old_fork" > "$sync_review/fork.patch"
   git diff "$old_base" "$new_upstream" > "$sync_review/upstream.patch"
   ```

   When both sides contain the same fix, upstream's text wins. Our features adapt to upstream, never the reverse. Preserve upstream IPC, session ownership and ordering; trace shared types and consumers beyond conflict markers. Keep only explicitly reviewed fork behavior. Do not reformat or fix unrelated code.

3. **Restore dependency floors after taking upstream's manifests and locks.** Start from upstream's dependency graph, then restore necessary fork constraints. Run the floor check immediately; use targeted package-manager updates for any regression, never textual lockfile edits or a general refresh.

   ```sh
   node bac/tools/check-security-floor.mjs
   # For each reviewed package or crate that fell below its floor:
   pnpm update PACKAGE --depth Infinity --lockfile-only
   pnpm --dir node-sidecar update PACKAGE --depth Infinity --lockfile-only
   cargo update --manifest-path src-tauri/Cargo.toml -p CRATE --precise VERSION
   pnpm install --frozen-lockfile
   pnpm --dir node-sidecar install --frozen-lockfile
   node bac/tools/check-security-floor.mjs
   python3 scripts/verify-vendored-glib.py
   ```

   Substitute actual names/versions only where an update is needed. Both backport checks are required: the floor validates the exact version, local source and patch table, while the verifier validates source bytes. Never lower an advisory floor to represent a backport. Review every open entry and record its disposition.

4. **Commit the merge and audit what it carries.** Stage only reviewed resolutions and verify there are no unresolved paths, then create the merge commit. Run both audits again after any integration follow-up commits.

   ```sh
   git diff --check
   test -z "$(git diff --name-only --diff-filter=U)"
   git commit
   node bac/tools/merge-audit.mjs "$old_base" "$old_fork" "$new_upstream" HEAD > "$sync_review/merge-audit.md"
   node bac/tools/compare-locks.mjs "$old_fork" "$new_upstream" HEAD > "$sync_review/lock-comparison.md"
   ```

   Review all four merge-audit categories: lost fork additions, additions absorbed by upstream, lines written during the merge, and upstream lines removed without a matching old fork removal. The audit includes deleted, own, vendor and lock paths and checks upstream content before calling a line absorbed. Trimmed line sets ignore order and duplicate counts; they are a review checklist, not semantic proof. `--json` supports filtering large output without discarding evidence.

   The lock comparison shows old-fork, upstream and merged versions for lowered maximums, newly introduced lower parallel versions and disappeared packages. Existing older parallel versions are not new regressions. Every reported row needs review, including packages with no advisory floor.

5. **Update the base and inventory.** Write the release identity, preserving the base file's shape:

   ```sh
   node --input-type=module - "$sync_tag" "$new_upstream" <<'JS'
   import { writeFileSync } from 'node:fs';
   const [tag, commit] = process.argv.slice(2);
   writeFileSync('bac/UPSTREAM_BASE.json', JSON.stringify({
     repository: 'https://github.com/tony1223/better-agent-terminal', tag, commit,
   }) + '\n');
   JS
   node bac/tools/check-upstream-inventory.mjs --update
   node bac/tools/check-upstream-inventory.mjs
   git diff -- bac/UPSTREAM_BASE.json bac/upstream-inventory.json
   ```

   Remove stale entries only after reviewing why they disappeared; reconcile own-directory collisions explicitly. Review every changed digest, reason, plan and hook declaration. `--update` cannot add a human explanation or resolve an ownership conflict. Lock counts remain informational, with native consistency and floor checks replacing digest pins.

6. **Run validation with an isolated home.** Prepare dependencies first. Keep package-manager/toolchain caches available when changing HOME; the example preserves standard cache locations without using the real home for application state.

   ```sh
   pnpm exec tsc --noEmit --pretty false
   pnpm run compile
   pnpm run build:tauri-sidecar
   node bac/tools/check-upstream-inventory.mjs
   node bac/tools/check-security-floor.mjs
   python3 scripts/verify-vendored-glib.py
   test_home=$(mktemp -d)
   mkdir -p "$test_home/tmp"
   COREPACK_HOME="${COREPACK_HOME:-${XDG_CACHE_HOME:-$HOME/.cache}/node/corepack}" \
   CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" \
   HOME="$test_home" USERPROFILE="$test_home" TMPDIR="$test_home/tmp" \
   XDG_CONFIG_HOME="$test_home/.config" XDG_DATA_HOME="$test_home/.local/share" bash <<'SH'
   set -e
   node --test bac/tools/tests/*.test.mjs
   node bac/tools/run-upstream-tests.mjs
   pnpm exec tsx tests/projects-sidebar.test.ts
   pnpm run test:fast-mode
   pnpm run test:session-reload
   cargo check --manifest-path src-tauri/Cargo.toml --workspace --locked
   pnpm run check:tauri-rust --locked --no-default-features --features headless
   pnpm run test:tauri-rust --locked --no-default-features --features headless --no-fail-fast -- --test-threads=1
   glib_test_dir=$(mktemp -d)
   cp -R src-tauri/vendor/glib-0.18.5/. "$glib_test_dir/"
   cargo test --manifest-path "$glib_test_dir/Cargo.toml" --release --lib variant_iter::tests
   SH
   validation_status=$?
   rm -rf "$test_home"
   test "$validation_status" -eq 0
   ```

   The test runner derives every `test:*` invocation in `verify:tauri-pre-ci`, always includes `test:sidecar`, continues after failures and reports each script's exit code. `--list` previews the selection. `--wrapper EXECUTABLE` invokes that executable with each script name as its sole argument instead of `pnpm run NAME`; use it for an environment-isolating test launcher.

   Run regression filters for behavior affected by the merge and the supported-platform packaging checks. Desktop and headless feature sets are separate gates. Namespace-dependent tests require a capable host; do not weaken their guards. Record failures and skips explicitly. `bac-ci.yml` covers inventory/floors, renderer, upstream script tests and Linux desktop checks; `pm-runtime-offline.yml` covers headless checks/tests, and `dependency-checks.yml` covers optimized GLib tests. Desktop CI has a provisional 30-minute timeout until hosted timing is measured.

7. **Review code scanning and prepare the PR.** Compare a flagged file with the new upstream tree:

   ```sh
   git diff --exit-code "$new_upstream" -- PATH_TO_FLAGGED_FILE
   git show -s --format='%H %P' HEAD
   git diff --stat "$new_upstream" HEAD
   ```

   The same alert number already open on main, paired with a file byte-identical to the new base, can identify a moved pre-existing alert. Compare the statement and previous/current locations too; changed line numbers alone prove neither novelty nor safety. A genuinely new alert in our own or modified code blocks merging. Existing alerts require an explicit owner disposition and are never dismissed automatically. Keep individual finding analysis in access-controlled review material until the owner authorizes disclosure.

   The PR description includes old fork/base/new upstream/final SHAs, merge-parent evidence, conflict decisions, automatic-merge review, audit dispositions, inventory totals and changed digests, manifest/floor decisions, validation commands with exit codes and skips, CI timing and remaining integration risks. Do not publish credentials, private deployment data or undisclosed finding details.

8. **Merge the sync PR with a merge commit, never squash or rebase.** Upstream commits must remain ancestors of main. After the PR is merged, refresh the tracking branch and verify ancestry:

   ```sh
   git fetch origin main
   git merge-base --is-ancestor "$sync_tag" origin/main
   ```

   Record exit 0 in the merge record. A clean content comparison cannot replace ancestry verification. Retain the review directory securely, then remove temporary copies when no longer needed.

## Record of the v3.2.18 merge

The old base was `b7419892fbc9946799b64cca24c2ec8c7fa15c42`, old fork `8cfaca253098c3f5eb01514a325cb12a4ae107fa`, upstream `07e80f56275075195d05d58cf67a208934874557`, and reviewed merged tree `86c65909cafd214ebf23622a2a2057fa607fa9f0`. There were 18 conflict files and 13 automatically merged files needing semantic review.

The history-page, message-dedupe, resume-recovery and transcript-window production helpers became byte-identical to upstream; complementary regression tests were retained. Shared history helpers had content conflicts despite earlier descriptions of add/add conflicts. Upstream Fast policy, runtime/catalog, rate-limit resume, translated recovery, legacy payload/args handling, archive queue ordering and start/end envelopes were preserved. The PM constructor needed a new upstream field initialized, and an unused App retry backoff remained after automatic merging until follow-up review removed it.

Taking upstream locks initially lost lock-only npm and Rust version raises. Targeted updates restored Hono/adapter, fast-uri, proxy-addr, qs, body-parser, ip-address, quinn-proto, rustls, serde_with and webpki while keeping runtime packages at upstream versions. Three defmt crates disappeared with optional jiff edges; they were documented rather than blindly restored. Root direct Zod was removed after import/peer-resolution checks, while its override retained 4.6.7.

Upstream's unmodified openai-direct-cleanup test had failed on fork main, demonstrating the need to run the upstream script list on every PR. Bounded archive/Codex reads, native-input accounting and transport regressions required explicit retention decisions. These integration lessons explain the audit and validation steps above; they do not authorize unrelated changes during the next merge.
