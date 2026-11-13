import { compareVersions, versionParts } from './common.mjs';
export const LOCKS = ['pnpm-lock.yaml', 'node-sidecar/pnpm-lock.yaml', 'src-tauri/Cargo.lock'];

// Deliberately parse generated lock formats, not arbitrary YAML/TOML. Unknown
// package records fail closed. Native frozen/locked checks validate full syntax
// and manifest consistency; these readers never treat snapshots as packages.
export function parsePnpm(text) {
  if (!/^lockfileVersion: ['"]?9\.0['"]?\s*$/m.test(text)) throw new Error('Expected pnpm lockfileVersion 9.0');
  const section = /^packages:\s*\n([\s\S]*?)(?=^\S|$(?![\s\S]))/m.exec(text)?.[1];
  if (section === undefined || !/^importers:/m.test(text) || !/^snapshots:/m.test(text)) throw new Error('Malformed pnpm lock: missing packages/importers/snapshots');
  const packages = [];
  let current;
  for (const line of section.split('\n')) {
    if (!line.trim() || /^\s*#/.test(line)) continue;
    if (/^  \S/.test(line)) {
      if (current && !current.resolution) throw new Error(`Missing resolution for ${current.name}`);
      const match = /^  (?:'([^']+)'|"([^"]+)"|([^'"].*?)):\s*(?:\{\})?\s*$/.exec(line);
      if (!match) throw new Error(`Malformed pnpm package key: ${line}`);
      const key = match[1] || match[2] || match[3];
      const at = key.lastIndexOf('@');
      if (at < 1) throw new Error(`Unsupported pnpm package key: ${key}`);
      const name = key.slice(0, at), version = key.slice(at + 1);
      versionParts(version);
      current = { name, version, source: 'registry', resolution: false };
      packages.push(current);
    } else if (!/^    \S|^ {5,}\S/.test(line)) {
      throw new Error(`Malformed pnpm package body: ${line}`);
    } else if (/^    resolution: \{.+\}$/.test(line)) current.resolution = true;
  }
  if (current && !current.resolution) throw new Error(`Missing resolution for ${current.name}`);
  if (!packages.length) throw new Error('Malformed pnpm lock: empty packages section');
  return packages;
}
export function parseCargo(text) {
  const blocks = text.split(/^\[\[package\]\]\s*$/m);
  if (!/^version = [34]\s*$/m.test(blocks.shift() ?? '') || !blocks.length) throw new Error('Malformed Cargo lock header/packages');
  return blocks.map(block => {
    const entry = {};
    let array = false;
    for (const raw of block.split('\n')) {
      const line = raw.trim();
      if (!line || line.startsWith('#')) continue;
      if (array) {
        if (line === ']') array = false;
        else if (!/^"(?:[^"\\]|\\.)*",?$/.test(line)) throw new Error(`Malformed Cargo dependency: ${line}`);
        continue;
      }
      if (line === 'dependencies = [') { array = true; continue; }
      const match = /^(name|version|source|checksum) = ("(?:[^"\\]|\\.)*")$/.exec(line);
      if (!match || Object.hasOwn(entry, match[1])) throw new Error(`Unsupported/malformed Cargo package field: ${line}`);
      entry[match[1]] = JSON.parse(match[2]);
    }
    if (array || !entry.name || !entry.version) throw new Error('Malformed Cargo package record');
    versionParts(entry.version);
    return entry;
  });
}
export const parseLock = (path, text) => path.endsWith('Cargo.lock') ? parseCargo(text) : parsePnpm(text);
export function versionsByName(entries) {
  const map = new Map();
  for (const { name, version } of entries) map.set(name, [...new Set([...(map.get(name) || []), version])].sort(compareVersions));
  return map;
}
