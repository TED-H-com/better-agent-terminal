import { X509Certificate, createHash } from 'node:crypto'

export const bannerFingerprint = /^  fingerprint: ((?:[A-F0-9]{2}:){31}[A-F0-9]{2})\r?$/m

export function ownedServerIdentity(banner, rawEnvelope) {
  const fingerprint = banner.match(bannerFingerprint)?.[1]
  if (!fingerprint) throw new Error('Owned server did not provide a complete SHA-256 fingerprint')
  const envelope = JSON.parse(rawEnvelope)
  // Fresh Linux stores an explicit plaintext envelope. Never decrypt private
  // storage or obtain trust material through an unauthenticated connection.
  if (envelope.enc !== false || typeof envelope.data !== 'string') {
    throw new Error('Owned server certificate is encrypted or unavailable; public certificate export is required')
  }
  const stored = JSON.parse(envelope.data)
  if (typeof stored.cert !== 'string') throw new Error('Owned server certificate PEM is unavailable')
  const certificate = new X509Certificate(stored.cert)
  if (certificate.fingerprint256 !== fingerprint) {
    throw new Error('Owned server certificate does not match its startup fingerprint')
  }
  // Retain only the single public certificate, never the adjacent private key.
  return Object.freeze({ certificate: certificate.toString(), fingerprint })
}

export function pinnedTlsOptions(identity) {
  if (!identity?.certificate || !/^(?:[A-F0-9]{2}:){31}[A-F0-9]{2}$/.test(identity.fingerprint)) {
    throw new Error('Owned server certificate and SHA-256 pin are required before connecting')
  }
  return {
    ca: identity.certificate,
    rejectUnauthorized: true,
    // Trust only the owned certificate. Its names need not match loopback:
    // this SHA-256 pin replaces hostname checking, not chain validation.
    checkServerIdentity(_hostname, peer) {
      const actual = peer.raw && createHash('sha256').update(peer.raw).digest('hex').toUpperCase().match(/../g).join(':')
      if (actual !== identity.fingerprint) {
        const error = new Error('Owned server TLS certificate SHA-256 pin mismatch')
        error.code = 'ERR_TLS_CERT_PIN_MISMATCH'
        return error
      }
    },
  }
}
