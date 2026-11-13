import assert from 'node:assert/strict'
import tls from 'node:tls'
import { createHash, X509Certificate } from 'node:crypto'
import { spawnSync } from 'node:child_process'
import { once } from 'node:events'
import { Client } from './client.mjs'
import { ownedServerIdentity } from './tls-identity.mjs'

function ephemeralCertificate() {
  // No fixture private keys on disk or in the repository. OpenSSL writes both
  // artifacts to a captured pipe; only the public certificate leaves this test.
  const result = spawnSync('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes',
    '-keyout', '-', '-out', '-', '-subj', '/CN=fixture.invalid',
    '-days', '1', '-addext', 'basicConstraints=critical,CA:FALSE'], {
    encoding: 'utf8', env: { PATH: '/usr/bin:/bin', OPENSSL_CONF: '/dev/null' },
  })
  assert.equal(result.status, 0, 'ephemeral test certificate generation must succeed')
  const key = result.stdout.match(/-----BEGIN PRIVATE KEY-----[\s\S]*?-----END PRIVATE KEY-----/)?.[0]
  const cert = result.stdout.match(/-----BEGIN CERTIFICATE-----[\s\S]*?-----END CERTIFICATE-----/)?.[0]
  assert.ok(key && cert, 'ephemeral key and certificate must be available')
  return { key, cert }
}

const first = ephemeralCertificate(), other = ephemeralCertificate()
const fingerprint = new X509Certificate(first.cert).fingerprint256
const banner = `  fingerprint: ${fingerprint}\n`
const envelope = JSON.stringify({ enc: false, data: JSON.stringify({ cert: first.cert }) })
const identity = ownedServerIdentity(banner, envelope)
assert.equal(identity.fingerprint, fingerprint)
assert.equal(identity.certificate.includes('PRIVATE KEY'), false)
assert.throws(() => ownedServerIdentity(banner, '{"enc":true,"data":"not-decrypted"}'), /encrypted or unavailable/)
assert.throws(() => ownedServerIdentity(banner, JSON.stringify({ enc: false, data: JSON.stringify({ cert: other.cert }) })), /does not match/)
assert.throws(() => ownedServerIdentity('  fingerprint: AA:BB\n', envelope), /complete SHA-256/)
console.log('PASS preconnection identity, encrypted-storage refusal, storage/banner mismatch and truncated pin refusal (4 checks)')

let applicationBytes = 0
const sockets = new Set(), clients = []
const server = tls.createServer(first, socket => {
  sockets.add(socket)
  socket.on('close', () => sockets.delete(socket))
  socket.on('error', () => {})
  let request = ''
  socket.on('data', bytes => {
    applicationBytes += bytes.length
    request += bytes.toString()
    if (!request.endsWith('\r\n\r\n')) return
    const key = request.match(/Sec-WebSocket-Key: (.+)\r\n/)?.[1]
    const accept = createHash('sha1').update(key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64')
    socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`)
  })
})
server.on('tlsClientError', () => {})
try {
  server.listen(0, '127.0.0.1'); await once(server, 'listening')
  const port = server.address().port
  const good = new Client(port, 'fixture', 'good-pin', true, identity); clients.push(good)
  await good.connect()
  assert.equal(good.socket.authorized, true)
  assert.ok(applicationBytes > 0, 'correct pin permits the HTTP upgrade despite fixture.invalid name')
  good.stop()

  const before = applicationBytes
  const wrong = new Client(port, 'fixture', 'wrong-pin', true, { ...identity, fingerprint: '00:'.repeat(31) + '00' }); clients.push(wrong)
  await assert.rejects(wrong.connect(), { code: 'ERR_TLS_CERT_PIN_MISMATCH' })
  assert.deepEqual(wrong.sent, [])
  assert.equal(applicationBytes, before, 'wrong pin must fail before even the HTTP upgrade')
  wrong.stop()

  const untrusted = new Client(port, 'fixture', 'wrong-ca', true, { ...identity, certificate: other.cert }); clients.push(untrusted)
  await assert.rejects(untrusted.connect(), { code: 'DEPTH_ZERO_SELF_SIGNED_CERT' })
  assert.deepEqual(untrusted.sent, [])
  assert.equal(applicationBytes, before, 'correct pin cannot bypass certificate trust validation')
  console.log('PASS validated exact certificate, wrong-pin refusal before frames and untrusted-certificate refusal (3 checks)')
} finally {
  for (const client of clients) client.stop()
  for (const socket of sockets) socket.destroy()
  if (server.listening) await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()))
}
