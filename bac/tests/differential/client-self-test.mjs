import assert from 'node:assert/strict'
import { gzipSync } from 'node:zlib'
import { Client } from './client.mjs'
import { normalize } from './normalize.mjs'
import { compare } from './comparator.mjs'
function packet(data, opcode = 1, final = true) {
  let header
  if (data.length < 126) header = Buffer.from([0, data.length])
  else if (data.length < 65536) { header = Buffer.alloc(4); header[1] = 126; header.writeUInt16BE(data.length, 2) }
  else { header = Buffer.alloc(10); header[1] = 127; header.writeBigUInt64BE(BigInt(data.length), 2) }
  header[0] = opcode | (final ? 128 : 0)
  return Buffer.concat([header, data])
}
for (const length of [125, 126, 127, 65535, 65536]) {
  const c = new Client(0, 'fixture', 'fixture')
  const frame = { data: 'x'.repeat(length - 11) }
  const bytes = packet(Buffer.from(JSON.stringify(frame)))
  for (let offset = 0; offset < bytes.length; offset += 7) {
    c.buffer = Buffer.concat([c.buffer, bytes.subarray(offset, offset + 7)]); c.parse()
  }
  assert.deepEqual(c.received, [frame])
}
const c = new Client(0, 'fixture', 'fixture')
const frame = { type: 'pong', id: 'fixture' }
const bytes = Buffer.concat([Buffer.from('BATGZIP1\0'), gzipSync(JSON.stringify(frame))])
c.buffer = Buffer.concat([packet(bytes.subarray(0, 10), 2, false), packet(bytes.subarray(10), 0)])
c.parse(); assert.deepEqual(c.received, [frame])
console.log('PASS WebSocket 125/126/127/65535/65536-byte boundaries and fragmented BATGZIP1 envelope (6 checks)')
const original = { clients: [{ sent: [{ type: 'invoke', id: 'meta', channel: 'claude:get-session-meta' }], received: [{ id: 'meta', result: { durationMs: 30, lastDataAt: null, unexpected: 1, inputState: { epoch: 'random', received: 2 } } }] }] }
const normalized = normalize(original, { dir: '/fixture/scratch', token: 'fixture' })
assert.equal(normalized.clients[0].received[0].result.durationMs, '<duration>')
assert.equal(normalized.clients[0].received[0].result.lastDataAt, null)
const changed = structuredClone(original)
changed.clients[0].received[0].result.inputState.extra = true
assert.equal(compare(normalized, normalize(changed, { dir: '/fixture/scratch', token: 'fixture' })).ok, false)
console.log('PASS field-scoped normalization, null preservation and unknown metadata detection (3 checks)')
