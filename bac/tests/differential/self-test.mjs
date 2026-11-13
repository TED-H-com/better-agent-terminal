import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { compare } from './comparator.mjs'
// Excerpt recorded from v3.2.18: auth, two workspace mutation events and an
// oversize invoke error. The companion transcript is copied then corrupted.
const recorded = JSON.parse(await readFile(new URL('./fixtures/comparator-transcript.json', import.meta.url), 'utf8'))
const seeds = {
  'extra field': x => { x.received[0].capabilities.unexpected = true },
  'changed value': x => { x.received[0].serverVersion = '99' },
  'missing frame': x => { x.received.splice(1, 1) },
  'reordered events': x => { [x.received[1], x.received[2]] = [x.received[2], x.received[1]] },
  'changed error code': x => { x.received[3].code = 'DIFFERENT_ERROR' },
}
assert.equal(compare(recorded, structuredClone(recorded)).ok, true)
for (const [name, seed] of Object.entries(seeds)) {
  const changed = structuredClone(recorded)
  seed(changed)
  const result = compare(recorded, changed)
  assert.equal(result.ok, false, name)
  assert.ok(result.differences.length, name)
  console.log(`PASS detects ${name}: ${result.differences.map(d => d.path).join(', ')}`)
}
const changed = structuredClone(recorded)
seeds['changed error code'](changed)
const entry = { id: 'error', reason: 'self-test only', ...compare(recorded, changed).differences[0] }
assert.equal(compare(recorded, changed, [entry]).ok, true)
assert.equal(compare(recorded, recorded, [entry]).ok, false, 'stale allowance fails')
assert.equal(compare(recorded, changed, [{ ...entry, fork: 'wrong' }]).ok, false, 'wrong allowance fails')
console.log('PASS exact allowlist; PASS stale allowlist; PASS mismatched allowlist; PASS identical transcripts (9 checks)')
const hashEntry = { id: 'large-string', reason: 'self-test', path: '/value',
  upstream: { $string: { length: 3, sha256: 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad' } }, fork: 'def' }
assert.equal(compare({ value: 'abc' }, { value: 'def' }, [hashEntry]).ok, true)
assert.equal(compare({ value: 'abd' }, { value: 'def' }, [hashEntry]).ok, false)
assert.equal(compare({ value: 'abc' }, { value: 'def', extra: true }, [hashEntry]).ok, false)
console.log('PASS exact string digest shape; PASS digest mismatch; PASS extra field outside allowance (12 checks total)')
await import('./client-self-test.mjs')
console.log('PASS 21 total comparator, transport and normalization checks')
