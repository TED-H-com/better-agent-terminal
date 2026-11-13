import { execFileSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

export function git(root, ...args) {
  return execFileSync('git', ['-C', root, '-c', 'core.quotePath=false', ...args], {
    encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, stdio: ['ignore', 'pipe', 'pipe'],
  });
}
export const isMain = url => process.argv[1] && url === pathToFileURL(process.argv[1]).href;
export function main(fn) {
  try { fn(); } catch (error) { console.error(error.message); process.exitCode = 1; }
}
export function versionParts(version) {
  const m = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$/.exec(version);
  if (!m) throw new Error(`Invalid resolved SemVer: ${version}`);
  return [...m.slice(1, 4).map(BigInt), m[4]?.split('.')];
}
export function compareVersions(a, b) {
  const x = versionParts(a), y = versionParts(b);
  for (let i = 0; i < 3; i++) if (x[i] !== y[i]) return x[i] < y[i] ? -1 : 1;
  if (!x[3] || !y[3]) return x[3] ? -1 : y[3] ? 1 : 0;
  for (let i = 0; i < Math.max(x[3].length, y[3].length); i++) {
    const u = x[3][i], v = y[3][i];
    if (u === v) continue;
    if (u === undefined || v === undefined) return u === undefined ? -1 : 1;
    const un = /^\d+$/.test(u), vn = /^\d+$/.test(v);
    if (un !== vn) return un ? -1 : 1;
    return (un ? BigInt(u) < BigInt(v) : u < v) ? -1 : 1;
  }
  return 0;
}
