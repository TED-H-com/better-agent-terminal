import { git, compareVersions, isMain, main } from './common.mjs';
import { LOCKS, parseLock, versionsByName } from './locks.mjs';

export function compareLocks(previous, upstream, merged) {
  const rows = [];
  for (const [name, old] of versionsByName(previous)) {
    const incoming = versionsByName(upstream).get(name) || [];
    const next = versionsByName(merged).get(name) || [];
    // Existing older parallel versions aren't new regressions. Report any newly
    // introduced lower version, and loss of a former highest version, separately.
    const lower = next.filter(v => !old.includes(v) && compareVersions(v, old.at(-1)) < 0);
    const highestFell = next.length > 0 && compareVersions(next.at(-1), old.at(-1)) < 0;
    if (lower.length || highestFell || !next.length) rows.push({ package: name, previous: old, upstream: incoming, merged: next, lower, status: !next.length ? 'disappeared' : 'lower' });
  }
  return rows;
}
if (isMain(import.meta.url)) main(() => {
  const args = process.argv.slice(2), json = args.at(-1) === '--json';
  if (json) args.pop();
  if (args.length !== 3) throw new Error('Usage: node bac/tools/compare-locks.mjs OLD_FORK NEW_UPSTREAM MERGED [--json]');
  for (const ref of args) git(process.cwd(), 'cat-file', '-e', `${ref}^{commit}`);
  const rows = LOCKS.flatMap(lock => compareLocks(...args.map(ref => parseLock(lock, git(process.cwd(), 'show', `${ref}:${lock}`)))).map(row => ({ lock, ...row })));
  if (json) console.log(JSON.stringify(rows, null, 2));
  else {
    console.log('| Lock | Package | Previous fork | Upstream | Merged | Review |\n|---|---|---|---|---|---|');
    for (const r of rows) console.log(`| ${r.lock} | ${r.package} | ${r.previous.join(', ')} | ${r.upstream.join(', ') || 'absent'} | ${r.merged.join(', ') || 'absent'} | ${r.status} |`);
    console.log(`\n${rows.filter(r => r.status === 'lower').length} lowered packages; ${rows.filter(r => r.status === 'disappeared').length} disappeared. Review every row; disappearance can be an intentional optional-edge change.`);
  }
});
