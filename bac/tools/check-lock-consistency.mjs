import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdtempSync, rmSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { isMain, main } from './common.mjs';

// No dependency installation or lifecycle scripts: pnpm only validates scratch
// copies of the lock/importer via its native frozen-lockfile implementation.
export function checkLocks(root) {
  for (const directory of ['', 'node-sidecar']) {
    const source = join(root, directory);
    if (!existsSync(join(source, 'pnpm-lock.yaml'))) continue;
    const scratch = mkdtempSync(join(tmpdir(), 'bac-lock-'));
    try {
      for (const file of ['package.json', 'pnpm-lock.yaml']) copyFileSync(join(source, file), join(scratch, file));
      execFileSync('pnpm', ['install', '--lockfile-only', '--frozen-lockfile', '--offline', '--ignore-scripts'], {
        cwd: scratch, stdio: 'pipe', env: { ...process.env, CI: 'true' },
      });
      console.log(`Consistent: ${directory ? `${directory}/` : ''}pnpm-lock.yaml (pnpm frozen, offline, lockfile-only)`);
    } catch (error) {
      throw new Error(`Lock/manifest consistency failed for ${directory || '.'}: ${error.stderr || error.stdout || error.message}; resolve with targeted pnpm update and frozen install.`);
    } finally { rmSync(scratch, { recursive: true, force: true }); }
  }
  if (existsSync(join(root, 'src-tauri/Cargo.lock'))) {
    // Cargo resolves the workspace with --locked; --no-deps would NOT check the
    // complete dependency graph. Metadata does not compile GTK or start the app.
    try {
      execFileSync('cargo', ['metadata', '--manifest-path', 'src-tauri/Cargo.toml', '--locked', '--offline', '--format-version', '1', '--filter-platform', 'x86_64-unknown-linux-gnu'], {
        cwd: root, stdio: 'pipe', maxBuffer: 64 * 1024 * 1024,
      });
      console.log('Consistent: src-tauri/Cargo.lock (cargo metadata --locked --offline)');
    } catch (error) {
      throw new Error(`Cargo lock/manifest validation failed: ${error.stderr || error.message}; populate the Cargo cache in CI, then run cargo metadata --manifest-path src-tauri/Cargo.toml --locked --offline --format-version 1 --filter-platform x86_64-unknown-linux-gnu; update deliberately if inconsistent.`);
    }
  }
}
if (isMain(import.meta.url)) main(() => checkLocks(process.cwd()));
