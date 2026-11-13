import { git, isMain, main } from './common.mjs';

const lines = text => new Set(text.split('\n').map(s => s.trim()).filter(Boolean));
const difference = (a, b) => new Set([...a].filter(x => !b.has(x)));
function content(root, revision, path) {
  // Absence is expected for additions/deletions; revision validity is checked first.
  const paths = git(root, 'ls-tree', '-r', '--name-only', '-z', revision, '--', path).split('\0');
  return paths.includes(path) ? lines(git(root, 'show', `${revision}:${path}`)) : new Set();
}
function sides(root, before, after, path) {
  const add = new Set(), remove = new Set();
  let hunk = false;
  for (const line of git(root, 'diff', '--no-ext-diff', '--no-textconv', '--no-renames', '--no-color', '-U0', before, after, '--', path).split('\n')) {
    if (line.startsWith('@@')) { hunk = true; continue; }
    if (!hunk) continue;
    const text = line.slice(1).trim();
    if (!text) continue;
    if (line.startsWith('+')) add.add(text);
    if (line.startsWith('-')) remove.add(text);
  }
  return { add, remove };
}
export function mergeAudit(root, base, fork, upstream, merged) {
  for (const ref of [base, fork, upstream, merged]) git(root, 'cat-file', '-e', `${ref}^{commit}`);
  const changed = (a, b) => git(root, 'diff', '--no-renames', '--name-only', '-z', a, b).split('\0').filter(Boolean);
  const paths = [...new Set([...changed(base, fork), ...changed(upstream, merged)])].sort();
  return paths.map(path => {
    const f = sides(root, base, fork, path), m = sides(root, upstream, merged, path);
    const u = content(root, upstream, path), carried = content(root, merged, path);
    const dropped = difference(f.add, m.add);
    return { path,
      lost: [...dropped].filter(line => !carried.has(line)),
      absorbed: [...dropped].filter(line => u.has(line) && carried.has(line)),
      writtenDuringMerge: [...difference(m.add, f.add)],
      upstreamRemovals: [...difference(m.remove, f.remove)],
    };
  });
}
export function checklist(rows) {
  const output = ['# Merge audit checklist', '', 'Line sets ignore whitespace, duplicates and order; these are review candidates, not semantic proof.', 'All fork-changed paths are included, including deletions, vendor files and locks; use compare-locks.mjs for versions.', ''];
  for (const row of rows) {
    output.push(`## ${row.path}`, '');
    for (const [key, label] of [['lost','Lost fork additions'],['absorbed','Absorbed by upstream'],['writtenDuringMerge','Written during merge'],['upstreamRemovals','Upstream lines removed without a matching fork removal']]) {
      output.push(`- [ ] ${label}: ${row[key].length}`);
      if (row[key].length) output.push('', '```text', ...row[key].map(line => line.replaceAll('```', '\u0060\u200b\u0060\u0060')), '```', '');
    }
    output.push('');
  }
  return output.join('\n');
}
if (isMain(import.meta.url)) main(() => {
  const args = process.argv.slice(2), json = args.at(-1) === '--json';
  if (json) args.pop();
  if (args.length !== 4) throw new Error('Usage: node bac/tools/merge-audit.mjs OLD_BASE OLD_FORK NEW_UPSTREAM MERGED [--json]');
  const rows = mergeAudit(process.cwd(), ...args);
  console.log(json ? JSON.stringify(rows, null, 2) : checklist(rows));
});
