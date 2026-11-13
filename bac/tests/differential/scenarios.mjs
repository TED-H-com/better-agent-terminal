import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { mkdir, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'
const connect = async (s, gzip = true) => { const c = await s.connect(gzip); assert.equal((await c.auth()).result, true); return c }
const workspace = name => JSON.stringify({ workspaces: [{ id: 'fixture-workspace', name }], terminals: [], activeWorkspaceId: 'fixture-workspace', activeTerminalId: null })
const events = (c, name) => c.received.filter(f => f.type === 'event' && f.channel === name)
export const scenarios = {
  async handshake(s) {
    const c = await connect(s)
    c.send({ type: 'ping', id: 'heartbeat' }); await c.response('heartbeat')
    await c.invoke('version', 'app:get-version')
    await c.invoke('legacy-args', 'app:get-version', undefined, { params: undefined, args: [] })
    const plain = await connect(s, false)
    await plain.invoke('plain-version', 'app:get-version')
  },
  async 'wrong-token'(s) {
    const c = await s.connect(); await c.auth({ token: 'intentionally-invalid-fixture' }); await c.closedByServer()
  },
  async 'missing-token'(s) {
    const c = await s.connect(); await c.auth({ token: undefined }); await c.closedByServer()
  },
  async 'protocol-mismatch'(s) {
    const c = await s.connect(); await c.auth({ protocols: ['fixture/unsupported'] }); await c.closedByServer()
  },
  async errors(s) {
    const c = await connect(s)
    await c.invoke('unknown', 'fixture:unknown')
    await c.invoke('no-channel', '')
    await c.invoke('wrong-path', 'fs:readFile', { path: 42 })
    await c.invoke('wrong-profile', 'profile:open', { profileId: 42 })
    await c.invoke('wrong-context', 'workspace:load', {}, { contextId: 42 })
    await c.invoke('wrong-save', 'workspace:save', { data: [] })
    await c.invoke('missing-session', 'claude:start-session', {})
  },
  async malformed(s) { const c = await connect(s, false); c.raw('{not-json'); await c.closedByServer() },
  async 'oversize-inbound'(s) {
    const c = await connect(s)
    c.send({ type: 'ping', id: 'oversize', padding: 'x'.repeat(64 * 1024 * 1024) })
    await c.closedByServer()
  },
  async profiles(s) {
    const c = await connect(s)
    await c.invoke('list', 'profile:list')
    await c.invoke('active', 'profile:get-active-ids')
    const opened = await c.invoke('open', 'profile:open', { profileId: 'default' })
    assert.ok(opened.result.contextId)
    const contextId = opened.result.contextId
    await c.invoke('status', 'profile:status', { contextId })
    await c.invoke('load', 'workspace:load', {}, { contextId })
    await c.invoke('mismatch', 'workspace:load', { profileId: 'foreign' }, { contextId })
    await c.invoke('root-only', 'workspace:summary', { targets: [] }, { contextId })
    const other = await connect(s)
    await other.invoke('foreign-context', 'workspace:load', {}, { contextId })
    await c.invoke('close', 'profile:close', { contextId })
    await c.invoke('closed-context', 'workspace:load', {}, { contextId })
    await c.invoke('unknown-profile', 'profile:open', { profileId: 'does-not-exist' })
  },
  async workspace(s) {
    const c = await connect(s), observer = await connect(s)
    await c.invoke('load-before', 'workspace:load')
    await c.invoke('save-one', 'workspace:save', { data: workspace('one') })
    await observer.until(() => events(observer, 'workspace:reload').length === 1)
    await c.invoke('load-after', 'workspace:load')
    await c.invoke('save-two', 'workspace:save', { data: workspace('two') })
    await observer.until(() => events(observer, 'workspace:reload').length === 2)
    await observer.invoke('observer-load', 'workspace:load')
  },
  async 'concurrent-workspace-rejections'(s) {
    const a = await connect(s), b = await connect(s)
    // Concurrent invalid saves exercise rejection without inventing an order
    // between independent mutation threads (successful races are not covered).
    const params = { data: '{invalid-workspace' }
    await Promise.all([a.invoke('save-a', 'workspace:save', params), b.invoke('save-b', 'workspace:save', params)])
    await a.invoke('load', 'workspace:load')
  },
  async filesystem(s) {
    const c = await connect(s)
    await c.invoke('home', 'fs:home')
    await c.invoke('read', 'fs:readFile', { path: join(s.repo, 'sample.txt') })
    await c.invoke('is-directory', 'fs:isDirectory', { path: s.repo })
    await c.invoke('mkdir', 'fs:mkdir', { parentPath: s.repo, name: 'child' })
    await c.invoke('list-dirs', 'fs:list-dirs', { path: s.repo })
    await c.invoke('readdir', 'fs:readdir', { path: s.repo })
    await c.invoke('missing-file', 'fs:readFile', { path: join(s.repo, 'absent') })
    await c.invoke('delete', 'fs:delete-path', { path: join(s.repo, 'child') })
    await c.invoke('branch', 'git:branch', { cwd: s.repo })
    await c.invoke('root', 'git:getRoot', { cwd: s.repo })
    await c.invoke('log', 'git:log', { cwd: s.repo, count: 1 })
    await writeFile(join(s.repo, 'sample.txt'), 'fixture changed\n')
    await c.invoke('status', 'git:status', { cwd: s.repo })
    await c.invoke('diff', 'git:diff', { cwd: s.repo })
    await c.invoke('diff-files', 'git:diff-files', { cwd: s.repo })
  },
  async agents(s) {
    const c = await connect(s)
    for (const channel of ['agent:get-supported-session-types', 'agent:list-presets']) await c.invoke(channel, channel)
    for (const channel of ['claude:list-sessions', 'agent:list-sessions']) await c.invoke(channel, channel, { cwd: s.repo })
    for (const channel of ['claude:get-session-meta', 'claude:get-session-state', 'claude:get-context-usage',
      'claude:get-supported-models', 'claude:get-supported-efforts', 'claude:get-supported-codex-sandbox-modes',
      'claude:get-supported-codex-approval-policies', 'claude:get-supported-commands', 'claude:get-supported-agents']) {
      await c.invoke(channel, channel, { sessionId: 'unknown-fixture' })
    }
    for (const channel of ['claude:get-supported-models', 'claude:get-supported-efforts']) {
      await c.invoke(`codex-${channel}`, channel, { sessionId: 'unknown-fixture', agentPreset: 'codex-agent' })
    }
    await c.invoke('send-unknown', 'claude:send-message', { sessionId: 'unknown-fixture', prompt: '', pmRuntimeId: 'fixture-pm', pmContextDigest: 'fixture-digest' })
  },
  async 'pm-runtime'(s) {
    const c = await connect(s)
    await c.invoke('pm-runtime', 'claude:get-pm-runtime', { sessionId: 'unknown-fixture' })
  },
  async 'claude-fake'(s) {
    const c = await connect(s)
    assert.equal((await c.invoke('start', 'claude:start-session', { sessionId: 'fixture-session', options: { cwd: s.repo, model: 'claude-sonnet-4-6' } })).result?.ok, true)
    await c.invoke('meta-before', 'claude:get-session-meta', { sessionId: 'fixture-session' })
    assert.equal((await c.invoke('send', 'claude:send-message', { sessionId: 'fixture-session', prompt: 'fixture prompt' })).result?.ok, true)
    await c.until(() => c.received.some(f => f.type === 'event' && /:turn-end$/.test(f.channel)), 15000)
    await c.invoke('meta-after', 'claude:get-session-meta', { sessionId: 'fixture-session' })
    await c.invoke('state', 'claude:get-session-state', { sessionId: 'fixture-session' })
    await c.invoke('stop', 'claude:stop-session', { sessionId: 'fixture-session' })
  },
  async 'codex-fake'(s) {
    const c = await connect(s)
    const sessionId = 'fixture-codex', options = { cwd: s.repo, agentPreset: 'codex-agent', model: 'gpt-5.4' }
    const started = await c.invoke('start', 'claude:start-session', { sessionId, options })
    assert.equal(started.type, 'invoke-result', JSON.stringify(started))
    await c.invoke('meta-before', 'claude:get-session-meta', { sessionId })
    const sent = await c.invoke('send', 'claude:send-message', { sessionId, prompt: 'fixture prompt', pmRuntimeId: 'unused-fixture', pmContextDigest: 'unused-fixture' })
    assert.equal(sent.type, 'invoke-result', JSON.stringify(sent))
    await c.until(() => c.received.some(f => f.type === 'event' && /:turn-end$/.test(f.channel)), 15000)
    await c.invoke('meta-after', 'claude:get-session-meta', { sessionId })
    await c.invoke('state', 'claude:get-session-state', { sessionId })
    await c.invoke('history', 'claude:client-resume', { sessionId, sdkSessionId: 'fixture-thread', options: { ...options, before: 1 } })
    await c.invoke('stop', 'claude:stop-session', { sessionId })
  },
  async 'retained-pm-start'(s) {
    const sessionId = 'retained-fixture'
    await mkdir(join(s.data, 'pm-runtimes', createHash('sha256').update(sessionId).digest('hex')), { recursive: true })
    const c = await connect(s)
    await c.invoke('retained-start', 'claude:start-session', { sessionId, options: { cwd: s.repo, agentPreset: 'codex-agent' } })
  },
  async 'retained-pm-resume'(s) {
    const sessionId = 'retained-fixture'
    await mkdir(join(s.data, 'pm-runtimes', createHash('sha256').update(sessionId).digest('hex')), { recursive: true })
    const c = await connect(s)
    await c.invoke('retained-resume', 'claude:resume-session', { sessionId, sdkSessionId: 'fixture-thread', options: { cwd: s.repo, agentPreset: 'codex-agent' } })
  },
  async 'retained-pm-send'(s) {
    const sessionId = 'retained-fixture'
    await mkdir(join(s.data, 'pm-runtimes', createHash('sha256').update(sessionId).digest('hex')), { recursive: true })
    const c = await connect(s)
    await c.invoke('retained-send', 'claude:send-message', { sessionId, prompt: '' })
  },
  async 'connection-limit'(s) {
    for (let i = 0; i < 64; i++) await connect(s)
    let refused = false
    try { await s.connect() } catch (err) {
      const c = s.clients.at(-1)
      if (!['ECONNRESET', 'EPIPE'].includes(err.code) && !err.message.includes('unexpected close')) throw err
      c.received.push({ transport: 'connection-refused-at-limit' })
      c.failure = undefined
      refused = true
    }
    assert.equal(refused, true, '65th connection must be refused')
    s.clients[0].send({ type: 'ping', id: 'still-alive' }); await s.clients[0].response('still-alive')
  },
  async 'invoke-limit'(s) {
    const c = await connect(s)
    const fifo = join(s.repo, 'blocked-read')
    const made = spawnSync('mkfifo', [fifo], { encoding: 'utf8' })
    assert.equal(made.status, 0, made.stderr)
    // Opening our FIFO blocks every reader until server teardown. A ping is
    // an inbound-processing barrier: all preceding invokes acquired a slot.
    for (let i = 0; i < 128; i++) c.send({ type: 'invoke', id: `blocked-${i}`, channel: 'fs:readFile', params: { path: fifo } })
    c.send({ type: 'ping', id: 'barrier' }); await c.response('barrier')
    const busy = await c.invoke('overflow', 'app:get-version')
    assert.equal(busy.type, 'invoke-error')
    assert.match(busy.error, /busy/)
  },
  async 'oversize-outbound'(s) {
    const c = await connect(s), observer = await connect(s)
    await c.invoke('save-large', 'workspace:save', { data: workspace('x'.repeat(5 * 1024 * 1024)) })
    await observer.until(() => observer.received.some(f => f.type === 'event'))
    await c.invoke('load-large', 'workspace:load')
  },
}
export async function drain(server) {
  // Finite observation window for unsolicited late events, never discards them.
  await delay(300)
  for (const c of server.clients) if (c.failure) throw c.failure
}
