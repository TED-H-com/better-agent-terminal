import { readFileSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { join } from 'node:path';
import { git, isMain, main } from './common.mjs';
import { LOCKS } from './locks.mjs';
import { checkLocks } from './check-lock-consistency.mjs';

const KINDS = ['hook', 'legacy-patch', 'dependency', 'test', 'deleted'];
const PLANS = ['keep', 'move-to-extension', 'upstream-as-fix', 'drop-when-upstream-has-it'];
export function basePaths(root, base) {
  return new Set(git(root, 'ls-tree', '-r', '--name-only', '-z', base).split('\0').filter(Boolean));
}
export function diffInfo(root, base, path) {
  const args = ['diff', '--no-ext-diff', '--no-textconv', '--no-renames', '--no-color', '--src-prefix=a/', '--dst-prefix=b/', '--diff-algorithm=myers'];
  const patch = git(root, ...args, '--binary', '--full-index', '--unified=0', base, '--', path);
  const stat = git(root, ...args, '--numstat', base, '--', path).trim().split('\t');
  return { added: stat[0] === '-' ? null : Number(stat[0]), removed: stat[1] === '-' ? null : Number(stat[1] || 0), digest: createHash('sha256').update(patch).digest('hex') };
}
export function inspect(root, { update = false } = {}) {
  const base = JSON.parse(readFileSync(join(root, 'bac/UPSTREAM_BASE.json'), 'utf8'));
  if (!/^[0-9a-f]{40}$/.test(base.commit)) throw new Error('UPSTREAM_BASE.commit must be a full SHA');
  try { git(root, 'cat-file', '-e', `${base.commit}^{commit}`); }
  catch { throw new Error(`Base ${base.commit} is missing locally. Run: git fetch --no-tags ${base.repository} ${base.commit}`); }
  const file = join(root, 'bac/upstream-inventory.json');
  const inventory = JSON.parse(readFileSync(file, 'utf8'));
  const upstream = basePaths(root, base.commit);
  // No rename detection: renaming an upstream file still deletes an owned path.
  const changed = new Set(git(root, 'diff', '--no-renames', '--name-only', '-z', base.commit, '--').split('\0').filter(p => upstream.has(p)));
  const current = new Set(git(root, 'ls-files', '-z', '--cached', '--others', '--exclude-standard').split('\0').filter(Boolean));
  const errors = [], stats = [], listed = new Set();
  for (const entry of inventory.upstream) {
    const { path, kind, plan } = entry;
    if (listed.has(path)) { errors.push(`Duplicate inventory entry: ${path}; keep one reason per path.`); continue; }
    listed.add(path);
    if (!KINDS.includes(kind) || !PLANS.includes(plan) || !entry.feature || !entry.reason || !entry.planReason) errors.push(`Incomplete explanation for ${path}; write kind, feature, reason, plan and planReason.`);
    if (!changed.has(path)) { errors.push(`Stale entry: ${path} no longer differs from the base; review and remove its inventory entry.`); continue; }
    const actual = diffInfo(root, base.commit, path);
    stats.push({ path, kind, feature: entry.feature, ...actual });
    const deleted = git(root, 'diff', '--no-renames', '--diff-filter=D', '--name-only', base.commit, '--', path).trim() !== '';
    if (deleted !== (kind === 'deleted')) errors.push(`Deletion classification changed: ${path}; review and set kind to ${deleted ? 'deleted' : 'its actual kind'}.`);
    if (LOCKS.includes(path)) {
      if (entry.digest !== null) errors.push(`Derived lock ${path}: digest must be null; native frozen/locked consistency checks replace the pin.`);
      // Counts are informational for derived locks and always printed from Git.
      if (update) Object.assign(entry, { added: actual.added, removed: actual.removed, digest: null });
    } else if (update) Object.assign(entry, actual);
    else if (entry.digest !== actual.digest || entry.added !== actual.added || entry.removed !== actual.removed) errors.push(`Diff drift: ${path}; review the patch and reason, then run node bac/tools/check-upstream-inventory.mjs --update and review the inventory diff.`);
  }
  for (const path of changed) if (!listed.has(path)) errors.push(`Uninventoried upstream path: ${path}; add a human-written inventory entry with feature, kind, reason and plan, then run --update.`);
  const own = new Set();
  for (const entry of inventory.own) {
    if (!entry.path || !entry.feature || own.has(entry.path)) errors.push(`Invalid/duplicate own-file entry: ${entry.path}`);
    own.add(entry.path);
    if (upstream.has(entry.path)) errors.push(`Own path collision: ${entry.path} now exists in the base; compare both implementations, relocate ours or adopt upstream, and reclassify before merging.`);
    else if (!current.has(entry.path)) errors.push(`Stale own path: ${entry.path}; review and remove its inventory entry.`);
  }
  for (const path of current) if (!path.startsWith('bac/') && !upstream.has(path) && !own.has(path)) errors.push(`Unlisted own path: ${path}; add it to the own list so future upstream collisions are detected.`);
  // Never partially rewrite a structurally invalid inventory.
  if (update && !errors.length) writeFileSync(file, `${JSON.stringify(inventory, null, 2)}\n`);
  return { errors, stats };
}
export function summary(stats) {
  const lines = [`Upstream files touched: ${stats.length}`];
  for (const dimension of ['kind', 'feature']) {
    const groups = new Map();
    for (const row of stats) {
      const value = groups.get(row[dimension]) || { files: 0, added: 0, removed: 0 };
      value.files++; value.added += row.added || 0; value.removed += row.removed || 0;
      groups.set(row[dimension], value);
    }
    for (const [name, v] of [...groups].sort()) lines.push(`${dimension} ${name}: ${v.files} files +${v.added}/-${v.removed}`);
  }
  return lines.join('\n');
}
if (isMain(import.meta.url)) main(() => {
  if (process.argv.slice(2).some(a => a !== '--update')) throw new Error('Usage: node bac/tools/check-upstream-inventory.mjs [--update]');
  const result = inspect(process.cwd(), { update: process.argv.includes('--update') });
  console.log(summary(result.stats));
  if (result.errors.length) throw new Error(result.errors.join('\n'));
  checkLocks(process.cwd());
  console.log('Upstream inventory passed.');
});
