import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, copyFileSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { checkLocks } from '../check-lock-consistency.mjs';

// Exercise the package managers themselves, not a duplicate range interpreter.
for (const directory of ['', 'node-sidecar']) test(`native frozen check rejects manifest drift: ${directory || 'root'}`, t => {
  const root = mkdtempSync(join(tmpdir(), 'bac-consistency-test-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const dest = join(root, directory); mkdirSync(dest, { recursive: true });
  copyFileSync(resolve(directory, 'pnpm-lock.yaml'), join(dest, 'pnpm-lock.yaml'));
  const manifest = JSON.parse(readFileSync(resolve(directory, 'package.json'), 'utf8'));
  manifest.dependencies['bac-fixture-missing'] = '1.0.0';
  writeFileSync(join(dest, 'package.json'), JSON.stringify(manifest));
  assert.throws(() => checkLocks(root), /Lock\/manifest consistency failed/);
});
test('native Cargo locked check rejects a changed workspace package version', t => {
  const root = mkdtempSync(join(tmpdir(), 'bac-consistency-test-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, 'src-tauri/src'), { recursive: true });
  writeFileSync(join(root, 'src-tauri/src/lib.rs'), '');
  writeFileSync(join(root, 'src-tauri/Cargo.toml'), '[package]\nname = "bac-fixture"\nversion = "0.2.0"\nedition = "2021"\n');
  writeFileSync(join(root, 'src-tauri/Cargo.lock'), 'version = 4\n[[package]]\nname = "bac-fixture"\nversion = "0.1.0"\n');
  assert.throws(() => checkLocks(root), /Cargo lock\/manifest validation failed/);
});
