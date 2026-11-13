// Small RFC6455 transport, following scripts/pm-server-smoke.py. Node built-ins
// avoid an undeclared transitive ws dependency. Supports v2 gzip as the desktop
// client offers it, masked client frames, control frames and fragmentation.
import tls from 'node:tls'
import { createHash, randomBytes } from 'node:crypto'
import { gzipSync, gunzipSync } from 'node:zlib'
import { setTimeout as delay } from 'node:timers/promises'
import assert from 'node:assert/strict'

export class Client {
  constructor(port, token, name, gzip = true) {
    this.port = port; this.token = token; this.name = name; this.gzip = gzip
    this.sent = []; this.received = []; this.buffer = Buffer.alloc(0)
    this.fragments = []; this.compressed = false; this.closed = false
  }
  async connect() {
    const key = randomBytes(16).toString('base64')
    // Only our owned loopback server; verify the WebSocket handshake below.
    this.socket = tls.connect({ host: '127.0.0.1', port: this.port, rejectUnauthorized: false })
    this.socket.on('error', err => {
      if (this.expectClose && ['ECONNRESET', 'EPIPE'].includes(err.code)) this.received.push({ transport: 'socket-error', code: err.code })
      else this.failure = err
    })
    this.socket.on('close', () => { this.closed = true })
    this.socket.on('data', data => {
      try {
        this.buffer = Buffer.concat([this.buffer, data])
        if (!this.upgraded) {
          const end = this.buffer.indexOf('\r\n\r\n')
          if (end < 0) { assert.ok(this.buffer.length < 16384); return }
          const lines = this.buffer.subarray(0, end).toString().split('\r\n')
          assert.match(lines[0], /^HTTP\/1\.1 101 /)
          const headers = Object.fromEntries(lines.slice(1).map(line => {
            const colon = line.indexOf(':'); return [line.slice(0, colon).toLowerCase(), line.slice(colon + 1).trim()]
          }))
          const accept = createHash('sha1').update(key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64')
          assert.equal(headers['sec-websocket-accept'], accept)
          // These generated values are normalized at their exact handshake fields.
          headers['sec-websocket-accept'] = '<websocket-accept>'
          this.handshake = { status: lines[0], headers }
          this.buffer = this.buffer.subarray(end + 4); this.upgraded = true
        }
        this.parse()
      } catch (err) { this.failure = err; this.socket.destroy() }
    })
    this.socket.once('secureConnect', () => {
      this.socket.write(`GET / HTTP/1.1\r\nHost: 127.0.0.1:${this.port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: ${key}\r\nSec-WebSocket-Version: 13\r\n\r\n`)
    })
    await this.until(() => this.upgraded, 10000)
    return this
  }
  parse() {
    while (this.buffer.length >= 2) {
      const first = this.buffer[0], second = this.buffer[1]
      assert.equal(first & 0x70, 0); assert.equal(second & 0x80, 0)
      let size = second & 127, offset = 2
      if (size === 126) { if (this.buffer.length < 4) return; size = this.buffer.readUInt16BE(2); offset = 4 }
      else if (size === 127) { if (this.buffer.length < 10) return; size = Number(this.buffer.readBigUInt64BE(2)); offset = 10 }
      assert.ok(size <= 70 * 1024 * 1024, 'bounded transport frame')
      if (this.buffer.length < offset + size) return
      const payload = this.buffer.subarray(offset, offset + size)
      this.buffer = this.buffer.subarray(offset + size)
      const op = first & 15
      if (op === 8) {
        this.received.push({ transport: 'close', code: payload.length >= 2 ? payload.readUInt16BE(0) : null, reason: payload.subarray(2).toString() })
        this.socket.end(); continue
      }
      if (op === 9 || op === 10) {
        this.received.push({ transport: op === 9 ? 'ping' : 'pong', data: payload.toString('base64') })
        if (op === 9) this.write(payload, 10)
        continue
      }
      assert.ok([0, 1, 2].includes(op))
      if (op !== 0) { assert.equal(this.fragments.length, 0); this.fragmentOp = op }
      this.fragments.push(payload)
      if (!(first & 0x80)) continue
      let bytes = Buffer.concat(this.fragments); this.fragments = []
      if (this.fragmentOp === 2) {
        assert.equal(bytes.subarray(0, 9).toString(), 'BATGZIP1\0')
        bytes = gunzipSync(bytes.subarray(9), { maxOutputLength: 70 * 1024 * 1024 })
      }
      this.received.push(JSON.parse(bytes.toString()))
    }
  }
  write(payload, op) {
    const mask = randomBytes(4)
    let head
    if (payload.length < 126) { head = Buffer.from([0x80 | op, 0x80 | payload.length]) }
    else if (payload.length < 65536) { head = Buffer.alloc(4); head[0] = 0x80 | op; head[1] = 254; head.writeUInt16BE(payload.length, 2) }
    else { head = Buffer.alloc(10); head[0] = 0x80 | op; head[1] = 255; head.writeBigUInt64BE(BigInt(payload.length), 2) }
    const masked = Buffer.from(payload)
    for (let i = 0; i < masked.length; i++) masked[i] ^= mask[i % 4]
    this.socket.write(Buffer.concat([head, mask, masked]))
  }
  send(frame) {
    this.sent.push(JSON.parse(JSON.stringify(frame)))
    const bytes = Buffer.from(JSON.stringify(frame))
    this.write(this.compressed ? Buffer.concat([Buffer.from('BATGZIP1\0'), gzipSync(bytes)]) : bytes, this.compressed ? 2 : 1)
  }
  raw(text) { this.sent.push({ transport: 'text', data: text }); this.write(Buffer.from(text), 1) }
  async until(predicate, timeout = 10000, allowClose = false) {
    const deadline = Date.now() + timeout
    while (!predicate()) {
      if (this.failure) throw this.failure
      if (this.closed && !allowClose) throw new Error(`${this.name}: unexpected close`)
      if (Date.now() >= deadline) throw new Error(`${this.name}: response timeout`)
      await delay(10)
    }
  }
  async auth(overrides = {}) {
    this.send({ type: 'auth', id: 'auth', token: this.token, protocols: ['bat-remote/v2', 'bat-remote/legacy-v1'],
      compression: this.gzip ? ['gzip'] : [], args: ['Differential fixture', { windowId: 'fixture-window',
        clientInfo: { deviceId: `fixture-${this.name}`, label: 'Differential fixture', platform: 'linux' } }], ...overrides })
    const result = await this.response('auth')
    this.compressed = result.compression === 'gzip'
    return result
  }
  async response(id) {
    await this.until(() => this.received.some(frame => frame.id === id))
    return this.received.find(frame => frame.id === id)
  }
  async invoke(id, channel, params = {}, extra = {}) {
    this.send({ type: 'invoke', id, channel, params, ...extra })
    return this.response(id)
  }
  async closedByServer() {
    this.expectClose = true
    await this.until(() => this.closed, 10000, true)
    this.received.push({ transport: 'eof' })
  }
  snapshot() { return { handshake: this.handshake, sent: this.sent, received: this.received } }
  stop() { this.socket?.destroy() }
}
