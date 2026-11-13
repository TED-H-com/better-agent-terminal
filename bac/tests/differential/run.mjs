#!/usr/bin/env node
import { parseArgs } from 'node:util'
import { readFile, mkdir, writeFile } from 'node:fs/promises'
import { resolve, join, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { gzipSync } from 'node:zlib'
import { createHash } from 'node:crypto'
import { buildPair } from './build.mjs'
import { startServer } from './server.mjs'
import { scenarios, drain } from './scenarios.mjs'
import { normalize, rules } from './normalize.mjs'
import { compare } from './comparator.mjs'
const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..')
const { values: options } = parseArgs({ options: {
  report: { type: 'string' }, scratch: { type: 'string' }, base: { type: 'string', default: join(root, 'bac/UPSTREAM_BASE.json') },
  'upstream-source': { type: 'string' }, 'fork-source': { type: 'string' },
  'upstream-bin': { type: 'string' }, 'fork-bin': { type: 'string' },
  'same-binary': { type: 'boolean', default: false }, repeat: { type: 'string', default: '1' },
  scenarios: { type: 'string' }, 'build-only': { type: 'boolean', default: false },
} })
if (!options.report || !options.scratch) throw new Error('Required: --report DIRECTORY --scratch DIRECTORY (disk-backed scratch)')
const report = resolve(options.report), scratch = resolve(options.scratch)
await mkdir(report, { recursive: true }); await mkdir(scratch, { recursive: true })
const base = JSON.parse(await readFile(options.base, 'utf8'))
if (!/^[0-9a-f]{40}$/.test(base.commit) || !/^v\d+\.\d+\.\d+(?:[-.a-z0-9]*)?$/.test(base.tag)) throw new Error('Invalid upstream reference')
let builds, owned
const started = Date.now()
// SIGINT/SIGTERM always clean up the process group we own before exiting.
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, async () => { await owned?.stop(); process.exit(signal === 'SIGINT' ? 130 : 143) })
try {
  if (options['upstream-bin']) {
    if (!options['upstream-source'] || (!options['same-binary'] && (!options['fork-bin'] || !options['fork-source']))) throw new Error('Explicit binaries require corresponding source trees')
    builds = [{ label: 'upstream', binary: resolve(options['upstream-bin']), source: resolve(options['upstream-source']) },
      { label: 'fork', binary: resolve(options['fork-bin'] || options['upstream-bin']), source: resolve(options['fork-source'] || options['upstream-source']) }]
  } else builds = buildPair({ root, scratch, report, base, upstreamSource: options['upstream-source'] })
  await writeFile(join(report, 'builds.json'), JSON.stringify({ base, builds }, null, 2))
  if (!options['build-only']) {
    const names = options.scenarios ? options.scenarios.split(',') : Object.keys(scenarios)
    for (const name of names) if (!scenarios[name]) throw new Error(`Unknown scenario: ${name}`)
    const allowed = options['same-binary'] ? [] : JSON.parse(await readFile(join(root, 'bac/tests/differential/allowed-differences.json'), 'utf8'))
    const repeat = Number(options.repeat)
    if (!Number.isSafeInteger(repeat) || repeat < 1) throw new Error('Invalid --repeat')
    const results = []
    for (let cycle = 1; cycle <= repeat; cycle++) {
      const pair = []
      for (const [index, label] of ['upstream', options['same-binary'] ? 'upstream-again' : 'fork'].entries()) {
        const transcript = {}
        pair.push(transcript)
        const build = builds[options['same-binary'] ? 0 : index]
        for (const name of names) {
          console.log(`cycle ${cycle} ${label}: ${name}`)
          owned = await startServer(build, scratch)
          try {
            await scenarios[name](owned); await drain(owned)
            const raw = { clients: owned.clients.map(c => c.snapshot()) }
            for (const client of raw.clients) for (const frame of client.received) {
              if (frame.type === 'auth-result' && frame.result === true && frame.serverVersion !== base.tag.slice(1)) {
                throw new Error(`Expected serverVersion ${base.tag.slice(1)}, received ${frame.serverVersion}`)
              }
            }
            transcript[name] = normalize(raw, owned)
            // Raw wire content except token and known scratch paths: normalized
            // transcripts retain all fields, including arbitrarily large ones.
            await writeFile(join(report, `${cycle}-${label}-${name}.json.gz`), gzipSync(JSON.stringify(transcript[name])))
          } catch (error) {
            await writeFile(join(report, `${cycle}-${label}-${name}-incomplete.json.gz`), gzipSync(JSON.stringify(normalize({ clients: owned.clients.map(c => c.snapshot()) }, owned))))
            await writeFile(join(report, `${cycle}-${label}-${name}-server.log`), owned.logs())
            throw error
          } finally { await owned.stop(); owned = undefined }
        }
      }
      const compared = compare(pair[0], pair[1], allowed)
      const counts = Object.fromEntries(names.map(name => [name, pair.map(t => t[name].clients.reduce((n, c) => n + c.sent.length + c.received.length, 0))]))
      results.push({ cycle, ...compared, counts })
      await writeFile(join(report, 'comparison.json'), JSON.stringify(compact({ results }), null, 2))
      const lines = results.flatMap(r => [
        `Cycle ${r.cycle}: ${r.ok ? 'PASS' : 'FAIL'}`,
        ...names.map(name => `${name}: frames ${r.counts[name].join(' / ')}, differences ${r.differences.filter(d => d.path.startsWith(`/${name}/`)).length}, allowances ${[...new Set(r.differences.filter(d => d.path.startsWith(`/${name}/`) && d.allowedBy).map(d => d.allowedBy))].join(', ') || 'none'}`),
        `Unexplained: ${r.differences.filter(d => !d.allowedBy).length}; stale: ${r.stale.join(', ') || 'none'}`,
      ])
      await writeFile(join(report, 'report.txt'), `${lines.join('\n')}\nElapsed: ${(Date.now() - started) / 1000}s\n`)
      console.log(lines.slice(-1)[0])
      if (!compared.ok) process.exitCode = 1
    }
    await writeFile(join(report, 'normalization-rules.json'), JSON.stringify(rules, null, 2))
  }
} catch (error) {
  await writeFile(join(report, 'failure.txt'), `${error.stack}\n`)
  console.error(error.stack); process.exitCode = 1
} finally { await owned?.stop() }
function compact(value) {
  if (typeof value === 'string' && value.length > 1000) return { $string: { length: value.length, sha256: createHash('sha256').update(value).digest('hex') } }
  if (Array.isArray(value)) return value.map(compact)
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, compact(v)]))
  return value
}
