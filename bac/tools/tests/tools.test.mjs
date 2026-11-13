import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, rmSync, renameSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { git, compareVersions } from '../common.mjs';
import { inspect, diffInfo, summary } from '../check-upstream-inventory.mjs';
import { checkFloor } from '../check-security-floor.mjs';
import { parseCargo, parsePnpm } from '../locks.mjs';
import { mergeAudit, checklist } from '../merge-audit.mjs';
import { compareLocks } from '../compare-locks.mjs';
import { testScripts } from '../run-upstream-tests.mjs';

function repository(t) {
  const root = mkdtempSync(join(tmpdir(), 'bac-tools-test-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  git(root, 'init', '-q');
  const write = (path, data) => { mkdirSync(join(root, path, '..'), { recursive: true }); writeFileSync(join(root, path), data); };
  const commit = () => {
    git(root, 'add', '.');
    git(root, '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', '-c', 'commit.gpgsign=false', 'commit', '-qm', 'fixture');
    return git(root, 'rev-parse', 'HEAD').trim();
  };
  write('source.txt', 'base\n'); write('untouched.txt', 'unchanged\n');
  const base = commit();
  write('bac/UPSTREAM_BASE.json', JSON.stringify({ repository: 'https://example.invalid/upstream', tag: 'fixture', commit: base }));
  const inventory = { upstream: [], own: [{ path: 'bac/', feature: 'maintenance' }] };
  const save = () => write('bac/upstream-inventory.json', JSON.stringify(inventory));
  const hook = (path = 'source.txt', kind = 'hook') => inventory.upstream.push({ path, kind, feature: 'fixture', reason: 'Fixture lifecycle hook', plan: 'keep', planReason: 'Needed here.', ...diffInfo(root, base, path) });
  save();
  return { root, write, commit, base, inventory, save, hook };
}
test('inventory: untouched base passes', t => assert.deepEqual(inspect(repository(t).root).errors, []));
test('inventory: hooked file passes; staged and unstaged content are checked', t => {
  const r = repository(t); r.write('source.txt', 'base\nhook\n'); r.hook(); r.save();
  assert.deepEqual(inspect(r.root).errors, []);
  git(r.root, 'add', 'source.txt'); r.write('source.txt', 'base\nstaged drift\n');
  assert.match(inspect(r.root).errors.join('\n'), /Diff drift/);
});
test('inventory: newly touched path fails, update cannot invent a reason', t => {
  const r = repository(t); r.write('untouched.txt', 'new edit\n');
  const before = readFileSync(join(r.root, 'bac/upstream-inventory.json'), 'utf8');
  assert.match(inspect(r.root, { update: true }).errors.join('\n'), /Uninventoried upstream path/);
  assert.equal(readFileSync(join(r.root, 'bac/upstream-inventory.json'), 'utf8'), before);
});
test('inventory: reverted entry fails', t => {
  const r = repository(t); r.write('source.txt', 'hook\n'); r.hook(); r.save(); r.write('source.txt', 'base\n');
  assert.match(inspect(r.root).errors.join('\n'), /Stale entry/);
});
test('inventory: deletion is counted and must be classified', t => {
  const r = repository(t); rmSync(join(r.root, 'source.txt')); r.hook('source.txt', 'deleted'); r.save();
  const result = inspect(r.root); assert.deepEqual(result.errors, []); assert.equal(result.stats[0].removed, 1);
  r.inventory.upstream[0].kind = 'hook'; r.save(); assert.match(inspect(r.root).errors.join('\n'), /Deletion classification/);
});
test('inventory: rename is an owned deletion plus an explicitly listed new path', t => {
  const r = repository(t); renameSync(join(r.root, 'source.txt'), join(r.root, 'renamed.txt'));
  assert.match(inspect(r.root).errors.join('\n'), /Uninventoried upstream path: source.txt/);
  r.hook('source.txt', 'deleted'); r.inventory.own.push({ path: 'renamed.txt', feature: 'fixture' }); r.save();
  assert.deepEqual(inspect(r.root).errors, []);
});
test('inventory: digest drift and --update refresh only existing entries', t => {
  const r = repository(t); r.write('source.txt', 'hook\n'); r.hook(); r.save(); r.write('source.txt', 'hook\nchange\n');
  assert.match(inspect(r.root).errors.join('\n'), /Diff drift/);
  assert.deepEqual(inspect(r.root, { update: true }).errors, []);
  assert.deepEqual(inspect(r.root).errors, []);
  assert.equal(JSON.parse(readFileSync(join(r.root, 'bac/upstream-inventory.json'))).upstream.length, 1);
});
test('inventory: future base collision fails', t => {
  const r = repository(t); r.inventory.own.push({ path: 'source.txt', feature: 'fixture' }); r.save();
  assert.match(inspect(r.root).errors.join('\n'), /Own path collision/);
});
test('inventory: new own files require a collision record, deleted own files require removal', t => {
  const r = repository(t); r.write('own.txt', 'own implementation\n');
  assert.match(inspect(r.root).errors.join('\n'), /Unlisted own path/);
  r.inventory.own.push({ path: 'own.txt', feature: 'fixture' }); r.save();
  assert.deepEqual(inspect(r.root).errors, []);
  git(r.root, 'add', 'own.txt'); rmSync(join(r.root, 'own.txt'));
  assert.match(inspect(r.root).errors.join('\n'), /Stale own path/);
});
test('inventory: duplicate entries and missing human explanations fail', t => {
  const r = repository(t); r.write('source.txt', 'hook\n'); r.hook();
  r.inventory.upstream[0].reason = ''; r.inventory.upstream.push({ ...r.inventory.upstream[0] }); r.save();
  const errors = inspect(r.root).errors.join('\n');
  assert.match(errors, /Duplicate inventory entry/); assert.match(errors, /Incomplete explanation/);
});
test('inventory: missing base prints exact fetch command', t => {
  const r = repository(t); r.write('bac/UPSTREAM_BASE.json', JSON.stringify({ repository: 'https://example.invalid/upstream', commit: '0'.repeat(40) }));
  assert.throws(() => inspect(r.root), /git fetch --no-tags https:\/\/example.invalid\/upstream 0000000000000000000000000000000000000000/);
});
test('inventory: own directory covers nested files but not a similar prefix', t => {
  const r = repository(t); r.write('extension/a.txt', 'a'); r.write('extension/nested/b.txt', 'b');
  r.inventory.own.push({ path: 'extension/', feature: 'fixture' }); r.save();
  assert.deepEqual(inspect(r.root).errors, []);
  r.write('extension-other/a.txt', 'outside');
  assert.match(inspect(r.root).errors.join('\n'), /Unlisted own path: extension-other\/a.txt/);
});
test('inventory: bac directory must have an explicit ownership entry', t => {
  const r = repository(t); r.inventory.own = []; r.save();
  assert.match(inspect(r.root).errors.join('\n'), /Unlisted own path: bac\/UPSTREAM_BASE.json/);
});
test('inventory: empty owned directory fails even if it exists on disk', t => {
  const r = repository(t); mkdirSync(join(r.root, 'empty'));
  r.inventory.own.push({ path: 'empty/', feature: 'fixture' }); r.save();
  assert.match(inspect(r.root).errors.join('\n'), /Empty own directory: empty\/; add a file or review and remove/);
});
for (const nested of ['extension/a.txt', 'extension/nested/']) {
  for (const reverse of [false, true]) test(`inventory: overlapping ownership fails (${nested}, reverse=${reverse})`, t => {
    const r = repository(t); r.write('extension/a.txt', 'a'); r.write('extension/nested/b.txt', 'b');
    const entries = ['extension/', nested].map(path => ({ path, feature: 'fixture' }));
    r.inventory.own.push(...(reverse ? entries.reverse() : entries)); r.save();
    assert.match(inspect(r.root).errors.join('\n'), /Overlapping own paths:.*remove the nested entry/);
  });
}
test('inventory: a new upstream file under an owned directory collides', t => {
  const r = repository(t); r.write('extension/upstream.txt', 'new upstream file');
  const newBase = r.commit();
  r.write('bac/UPSTREAM_BASE.json', JSON.stringify({ repository: 'fixture', commit: newBase }));
  r.write('extension/ours.txt', 'our separate file');
  // The fixture base contains its metadata; only the directory under test is owned.
  r.inventory.own = [{ path: 'extension/', feature: 'fixture' }]; r.save();
  assert.match(inspect(r.root).errors.join('\n'), /Own path collision: extension\/ covers upstream path extension\/upstream.txt/);
});
test('inventory: malformed own paths and duplicate owners fail', t => {
  const r = repository(t);
  r.inventory.own.push({ path: '../outside/', feature: 'fixture' }, { path: 'bac/', feature: 'fixture' }); r.save();
  const errors = inspect(r.root).errors.join('\n');
  assert.match(errors, /Invalid own path: ..\/outside\//); assert.match(errors, /Duplicate own path: bac\//);
});
test('inventory: declared hook marker passes and summary counts marked/unmarked hook entries', t => {
  const r = repository(t); r.write('source.txt', 'base\n/* bac-hook: first-hook*/\n'); r.hook();
  r.inventory.upstream[0].hooks = ['first-hook'];
  r.write('untouched.txt', 'changed\n'); r.hook('untouched.txt'); r.save();
  const result = inspect(r.root);
  assert.deepEqual(result.errors, []);
  assert.match(summary(result.stats), /Hook entries: 1 marked, 1 unmarked/);
});
test('inventory: undeclared marker fails and --update cannot accept it', t => {
  const r = repository(t); r.write('source.txt', '// bac-hook: undeclared\n'); r.hook(); r.save();
  assert.match(inspect(r.root, { update: true }).errors.join('\n'), /Undeclared hook marker: undeclared in source.txt; review it and add the ID/);
});
test('inventory: declared marker missing from content fails', t => {
  const r = repository(t); r.write('source.txt', 'hook without marker\n'); r.hook();
  r.inventory.upstream[0].hooks = ['missing']; r.save();
  assert.match(inspect(r.root).errors.join('\n'), /Missing hook marker: missing in source.txt; restore its bac-hook comment/);
});
test('inventory: a marker in an uninventoried upstream file fails as a new difference', t => {
  const r = repository(t); r.write('untouched.txt', '// bac-hook: unlisted\n');
  assert.match(inspect(r.root).errors.join('\n'), /Uninventoried upstream path: untouched.txt/);
});
for (const id of ['Upper', 'under_score', '-leading', 'with.dot', '']) test(`inventory: invalid hook ID ${JSON.stringify(id)} fails`, t => {
  const r = repository(t); r.write('source.txt', `// bac-hook: ${id}\n`); r.hook();
  r.inventory.upstream[0].hooks = [id]; r.save();
  const errors = inspect(r.root).errors.join('\n');
  assert.match(errors, /Invalid hook ID/); assert.match(errors, /Invalid hook marker/);
});
test('inventory: hook list must be an array', t => {
  const r = repository(t); r.write('source.txt', 'hook\n'); r.hook(); r.inventory.upstream[0].hooks = 'bad'; r.save();
  assert.match(inspect(r.root).errors.join('\n'), /Invalid hooks list: source.txt; declare an array/);
});
test('inventory: hook IDs cannot be declared in two files', t => {
  const r = repository(t);
  for (const path of ['source.txt', 'untouched.txt']) {
    r.write(path, '// bac-hook: reused\n'); r.hook(path); r.inventory.upstream.at(-1).hooks = ['reused'];
  }
  r.save(); const errors = inspect(r.root).errors.join('\n');
  assert.match(errors, /Duplicate hook ID: reused.*repository-unique ID/);
  assert.match(errors, /Duplicate hook marker: reused.*exactly once/);
});
test('inventory: duplicate declarations and repeated markers within one file fail', t => {
  const r = repository(t); r.write('source.txt', '// bac-hook: repeated\n// bac-hook: repeated\n'); r.hook();
  r.inventory.upstream[0].hooks = ['repeated', 'repeated']; r.save();
  const errors = inspect(r.root).errors.join('\n');
  assert.match(errors, /Duplicate hook ID: repeated/); assert.match(errors, /Duplicate hook marker: repeated/);
});
const floorEntry = { package: 'sample', minimum: '1.2.0', lockfiles: ['pnpm-lock.yaml'], status: 'enforced', advisories: [{ id: 'GHSA-fixture', severity: 'high' }] };
function floorResult(versions, entry = floorEntry, read) {
  return checkFloor({ packages: [entry] }, new Map([[entry.lockfiles[0], versions.map(v => typeof v === 'string' ? { name: entry.package, version: v } : v)]]), read);
}
test('floor: below, at floor, multiple versions, absent package, open', () => {
  assert.match(floorResult(['1.1.0']).errors[0], /sample 1.1.0.*1.2.0.*GHSA-fixture.*pnpm update/);
  assert.deepEqual(floorResult(['1.2.0']).errors, []);
  assert.equal(floorResult(['1.3.0', '1.1.0']).errors.length, 1);
  assert.match(floorResult([]).notes[0], /ABSENT/);
  const open = floorResult(['1.0.0'], { ...floorEntry, status: 'open', reason: 'Major migration required' });
  assert.deepEqual(open.errors, []); assert.match(open.notes[0], /OPEN.*Major migration/);
});
test('floor: Rust remediation preserves context and links to the constrained update procedure', () => {
  const entry = { ...floorEntry, lockfiles: ['src-tauri/Cargo.lock'] };
  assert.deepEqual(floorResult(['1.1.0'], entry).errors, [
    'src-tauri/Cargo.lock: sample 1.1.0 is below floor 1.2.0 (GHSA-fixture); follow bac/SYNC.md#raise-a-rust-dependency-floor, then recheck; never edit the lock by hand.',
  ]);
});
for (const lock of ['pnpm-lock.yaml', 'node-sidecar/pnpm-lock.yaml']) test(`floor: pnpm remediation remains unchanged for ${lock}`, () => {
  const entry = { ...floorEntry, lockfiles: [lock] };
  const directory = lock.startsWith('node-sidecar/') ? '--dir node-sidecar ' : '';
  assert.deepEqual(floorResult(['1.1.0'], entry).errors, [
    `${lock}: sample 1.1.0 is below floor 1.2.0 (GHSA-fixture); run pnpm ${directory}update sample --depth Infinity --lockfile-only, review manifest constraints if necessary, then recheck; never edit the lock by hand.`,
  ]);
});
const backport = { ...floorEntry, package: 'glib', minimum: '0.20.0', status: 'backported', lockfiles: ['src-tauri/Cargo.lock'], backport: { version: '0.18.5', path: 'vendor/glib-0.18.5', manifest: 'src-tauri/Cargo.toml', verifier: 'python3 scripts/verify-vendored-glib.py' } };
const patchTable = () => '[patch.crates-io]\nglib = { path = "vendor/glib-0.18.5" }\n';
test('floor: exact local backport present', () => assert.deepEqual(floorResult(['0.18.5'], backport, patchTable).errors, []));
for (const [label, versions] of [
  ['registry at same version', [{ name: 'glib', version: '0.18.5', source: 'registry+fixture', checksum: 'abc' }]],
  ['another version', ['0.20.0']], ['two entries', ['0.18.5', '0.18.5']], ['absent', []],
  ['checksum without source', [{ name: 'glib', version: '0.18.5', checksum: 'abc' }]],
]) test(`floor: backport lost (${label})`, () => assert.match(floorResult(versions, backport, patchTable).errors[0], /backport was lost.*verify-vendored-glib.py/));
test('floor: wrong patch table fails', () => assert.match(floorResult(['0.18.5'], backport, () => '[patch.crates-io]\nglib = { path = "elsewhere" }').errors[0], /backport was lost/));
const pnpmLock = versions => `lockfileVersion: '9.0'\nimporters:\n  .: {}\npackages:\n${versions.map(v => `  'sample@${v}':\n    resolution: {integrity: fixture}\n`).join('')}snapshots:\n  sample@1.2.0: {}\n`;
test('lock readers: pnpm scoped names, snapshots excluded, all versions', () => {
  const text = pnpmLock(['1.1.0','1.2.0']).replaceAll('sample@', '@scope/sample@');
  const entries = parsePnpm(text);
  assert.equal(entries.length, 2); assert.equal(entries[0].name, '@scope/sample');
});
test('lock readers: malformed locks fail closed', () => {
  for (const text of ['broken', pnpmLock(['garbage']), pnpmLock(['1.2.0']).replace('resolution:', 'oops:'), pnpmLock(['1.2.0']).replace("'sample@1.2.0':", "'sample@1.2.0'")]) assert.throws(() => parsePnpm(text));
  for (const text of ['broken', 'version = 4\n[[package]]\nname = "x"\n', 'version = 4\n[[package]]\nname = "x"\nversion = "wat"']) assert.throws(() => parseCargo(text));
});
test('lock readers: Cargo preserves registry/local provenance and duplicate entries', () => {
  const text = 'version = 4\n[[package]]\nname = "glib"\nversion = "0.18.5"\n';
  const entries = parseCargo(`${text}[[package]]\nname = "glib"\nversion = "0.18.5"\nsource = "registry+fixture"\nchecksum = "x"\n`);
  assert.equal(entries.length, 2); assert.equal(entries[0].source, undefined); assert.equal(entries[1].source, 'registry+fixture');
});
test('SemVer compares numeric and prerelease components', () => {
  for (const [a,b] of [['1.2.9','1.2.10'], ['1.0.0-alpha.2','1.0.0-alpha.10'], ['1.0.0-rc.1','1.0.0'], ['0.9.0','1.0.0']]) assert.equal(compareVersions(a,b), -1);
  assert.equal(compareVersions('1.0.0+build.1','1.0.0+build.2'), 0);
});
test('merge audit: lost, absorbed, written, and upstream removals; deletions and added files', t => {
  const r = repository(t);
  r.write('source.txt', 'base\nlost\nabsorbed\nretained\n');
  r.write('fork-only.txt', 'own\n'); const fork = r.commit();
  git(r.root, 'checkout', '-q', r.base);
  r.write('source.txt', 'base\nabsorbed\nupstream-only\n'); const upstream = r.commit();
  r.write('source.txt', 'base\nabsorbed\nretained\nmerge-adaptation\n'); const merged = r.commit();
  const rows = mergeAudit(r.root,r.base,fork,upstream,merged);
  const row = rows.find(r => r.path === 'source.txt');
  assert.deepEqual(row.lost,['lost']); assert.deepEqual(row.absorbed,['absorbed']);
  assert.deepEqual(row.writtenDuringMerge,['merge-adaptation']); assert.deepEqual(row.upstreamRemovals,['upstream-only']);
  assert.deepEqual(rows.find(r => r.path === 'fork-only.txt').lost,['own']);
  assert.match(checklist(rows), /- \[ \] Lost fork additions/);
});
test('lock comparison: downgrades, new parallel older version, removed and unchanged', () => {
  const entries = list => list.map(([name,version]) => ({name,version}));
  const old = entries([['a','2.0.0'],['b','1.0.0'],['b','2.0.0'],['c','1.0.0'],['d','1.0.0']]);
  const next = entries([['a','1.9.0'],['b','1.0.0'],['b','2.0.0'],['b','1.5.0'],['d','1.0.0']]);
  const rows = compareLocks(old,entries([['a','1.8.0']]),next);
  assert.deepEqual(rows.map(r => r.package), ['a','b','c']);
  assert.deepEqual(rows[0].upstream,['1.8.0']); assert.equal(rows[2].status,'disappeared');
  assert.deepEqual(compareLocks(old, old, old), []);
});
test('upstream test discovery follows new scripts and deduplicates sidecar', () => {
  assert.deepEqual(testScripts({ 'verify:tauri-pre-ci': 'pnpm run compile && pnpm run test:new && pnpm run test:sidecar', 'test:new':'true', 'test:sidecar':'true' }), ['test:new','test:sidecar']);
  assert.throws(() => testScripts({'verify:tauri-pre-ci':'pnpm run test:missing'}), /Missing test script/);
});
