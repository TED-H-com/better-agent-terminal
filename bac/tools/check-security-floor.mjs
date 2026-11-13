import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { compareVersions, isMain, main, versionParts } from './common.mjs';
import { LOCKS, parseLock } from './locks.mjs';

export function checkFloor(floor, locks, read = path => readFileSync(path, 'utf8')) {
  const errors = [], notes = [], seen = new Set();
  for (const entry of floor.packages) {
    const { package: name, minimum, status, lockfiles, advisories } = entry;
    if (!name || !lockfiles?.length || !advisories?.length || !['enforced', 'open', 'backported'].includes(status)) throw new Error('Malformed security floor entry');
    versionParts(minimum);
    const ids = advisories.map(a => a.id).join(', ');
    for (const lock of lockfiles) {
      const key = `${lock}:${name}`;
      if (seen.has(key)) throw new Error(`Duplicate floor: ${key}`);
      seen.add(key);
      if (!LOCKS.includes(lock) || !locks.has(lock)) throw new Error(`Unknown or missing lock: ${lock}`);
      const found = locks.get(lock).filter(p => p.name === name);
      if (status === 'backported') {
        const backport = entry.backport;
        if (!backport?.version || !backport?.path || !backport?.manifest || !backport?.verifier) throw new Error(`Missing backport provenance: ${name}`);
        // Also ensure the declared patch still points at the reviewed directory.
        const manifest = read(backport.manifest);
        const table = /^\[patch\.crates-io\]\s*\n([\s\S]*?)(?=^\[|$(?![\s\S]))/m.exec(manifest)?.[1] || '';
        const patch = table.split('\n').some(line => line.trim() === `${name} = { path = "${backport.path}" }`);
        if (found.length !== 1 || found[0].version !== backport.version || Object.hasOwn(found[0], 'source') || Object.hasOwn(found[0], 'checksum') || !patch) {
          errors.push(`${lock}: ${name} backport was lost (${found.map(p => `${p.version} ${p.source || 'local'}`).join(', ') || 'absent'}); require exactly one local ${backport.version} via ${backport.manifest} [patch.crates-io] ${backport.path}; restore it and run ${backport.verifier} (${ids}).`);
        } else notes.push(`BACKPORTED ${name} ${backport.version}; separately run ${backport.verifier}`);
      } else if (status === 'open') {
        if (!entry.reason) throw new Error(`Open floor requires a reason: ${name}`);
        notes.push(`OPEN ${name} in ${lock}: ${found.map(p => p.version).join(', ') || 'absent'}; floor ${minimum}; ${entry.reason} (${ids})`);
      } else {
        if (!found.length) notes.push(`ABSENT ${name} in ${lock}; keep floor to catch reintroduction`);
        for (const pkg of found) if (compareVersions(pkg.version, minimum) < 0) {
          const remediation = lock.endsWith('Cargo.lock')
            ? 'follow bac/SYNC.md#raise-a-rust-dependency-floor, then recheck'
            : `run pnpm ${lock.startsWith('node-sidecar/') ? '--dir node-sidecar ' : ''}update ${name} --depth Infinity --lockfile-only, review manifest constraints if necessary, then recheck`;
          errors.push(`${lock}: ${name} ${pkg.version} is below floor ${minimum} (${ids}); ${remediation}; never edit the lock by hand.`);
        }
      }
    }
  }
  return { errors, notes };
}
export function run(root = process.cwd()) {
  const read = path => readFileSync(resolve(root, path), 'utf8');
  const locks = new Map(LOCKS.map(path => [path, parseLock(path, read(path))]));
  const result = checkFloor(JSON.parse(read('bac/security-floor.json')), locks, read);
  for (const note of result.notes) console.log(note);
  if (result.errors.length) throw new Error(result.errors.join('\n'));
  console.log('Security floor passed (backport source verifier is a separate required check).');
}
if (isMain(import.meta.url)) main(() => run());
