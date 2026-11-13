import * as assert from 'node:assert/strict'
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const { dispatch } = await import('../src/server.mjs')
const { __setSdkOverrideForTests } = await import('../src/lib/sdk-loader.mjs')
const { __setSendEventForTests } = await import('../src/lib/protocol.mjs')
const { ensureSession, sessions, buildSessionMeta } = await import('../src/lib/state.mjs')
const { closeLiveQuery } = await import('../src/handlers/claude-send.mjs')
const { isFastModeDebugEnabled, readFastModePolicy, supportsClaudeFastMode } = await import('../src/lib/fast-mode.mjs')

const root = mkdtempSync(join(tmpdir(), 'bat-fast-mode-'))
const savedEnv = { BAT_DEBUG: process.env.BAT_DEBUG, BAT_SIDECAR_DATA_DIR: process.env.BAT_SIDECAR_DATA_DIR }
process.env.BAT_SIDECAR_DATA_DIR = root
let requestId = 1
const events = []
const options = []
let sdkFastState = null
__setSendEventForTests((name, data) => events.push({ name, data }))
__setSdkOverrideForTests({
  query({ prompt, options: opts }) {
    options.push(opts)
    return (async function* () {
      for await (const _message of prompt) {
        yield { type: 'system', subtype: 'init', session_id: 'fake-fast-sdk', model: opts.model,
          fast_mode_state: sdkFastState || (opts.settings.fastMode ? 'on' : 'off'),
          fast_mode_disabled_reason: sdkFastState === 'off' ? 'extra_usage_disabled' : undefined }
        yield { type: 'result', subtype: 'success', session_id: 'fake-fast-sdk', result: 'done', num_turns: 1 }
      }
    })()
  },
})
const call = (method, params) => dispatch({ jsonrpc: '2.0', id: requestId++, method, params })
const policy = (allowed, epoch = 0) => writeFileSync(join(root, 'settings.json'), JSON.stringify({ allowFastMode: allowed, fastModeEpoch: epoch }))
const s = ensureSession('fast-test')
s.options = { cwd: root }
s.active = true
s.model = 'claude-opus-5-5'
async function turn() {
  const reply = await call('claude.sendMessage', { sessionId: 'fast-test', prompt: 'fake test only' })
  assert.equal(reply.error, undefined)
  assert.equal(reply.result.ok, true)
  for (let i = 0; i < 100 && s.streaming; i++) await new Promise(resolve => setImmediate(resolve))
  assert.equal(s.streaming, false)
}
async function setFast(enabled) { return call('claude.setFastMode', { sessionId: 'fast-test', enabled }) }

try {
  for (const value of [undefined, '0', 'false', 'yes']) assert.equal(isFastModeDebugEnabled({ BAT_DEBUG: value }), false)
  for (const value of ['1', 'true', 'TRUE']) assert.equal(isFastModeDebugEnabled({ BAT_DEBUG: value }), true)
  assert.equal(supportsClaudeFastMode('claude-opus-5-5[1m]'), true)
  for (const model of ['claude-opus-4-7', 'claude-sonnet-4-6', 'opus', 'custom']) assert.equal(supportsClaudeFastMode(model), false)

  policy(true)
  delete process.env.BAT_DEBUG
  assert.equal(readFastModePolicy().allowed, false, 'saved opt-in cannot bypass the debug gate')
  assert.equal(buildSessionMeta(s).fastModeAllowed, false, 'remote metadata must expose the host debug gate')
  assert.match(buildSessionMeta(s).fastModeDisabledReason, /BAT_DEBUG=1 on the host/)
  assert.match((await setFast(true)).error.message, /BAT_DEBUG/)
  s.fastMode = true
  s.fastModeEpoch = 0
  await turn()
  assert.equal(options.at(-1).settings.fastMode, false, 'no-debug startup must override inherited CLI Fast')
  assert.equal(buildSessionMeta(s).fastMode, false)

  process.env.BAT_DEBUG = '1'
  policy(false)
  assert.equal(buildSessionMeta(s).fastModeAllowed, false)
  assert.match(buildSessionMeta(s).fastModeDisabledReason, /host settings first/)
  assert.match((await setFast(true)).error.message, /settings/)
  policy(true)
  assert.equal(buildSessionMeta(s).fastModeAllowed, true)
  await turn()
  assert.equal(options.at(-1).settings.fastMode, false, 'global opt-in must not enable a conversation')

  const enabled = await setFast(true)
  assert.equal(enabled.result.fastModeAllowed, true)
  assert.equal(enabled.result.fastMode, true)
  assert.equal(enabled.result.fastModeState, 'pending')
  await turn()
  assert.equal(options.at(-1).settings.fastMode, true)
  assert.equal(options.at(-1).resume, 'fake-fast-sdk', 'changing Fast retains the conversation')
  assert.equal(buildSessionMeta(s).fastModeState, 'on', 'CLI status confirms actual speed')
  sdkFastState = 'cooldown'
  await turn()
  assert.equal(buildSessionMeta(s).fastMode, true, 'temporary cooldown retains the explicit opt-in')
  assert.equal(buildSessionMeta(s).fastModeState, 'cooldown')
  sdkFastState = 'off'
  await turn()
  assert.equal(buildSessionMeta(s).fastMode, false, 'runtime rejection must not display Fast as enabled')
  assert.equal(buildSessionMeta(s).fastModeDisabledReason, 'extra_usage_disabled')
  sdkFastState = null
  await setFast(true)
  await turn()
  const beforeRevocation = options.length
  policy(false, 1)
  policy(true, 1)
  assert.equal(buildSessionMeta(s).fastMode, false, 're-enabling the gate cannot restore a revoked opt-in')
  await turn()
  assert.equal(options.length, beforeRevocation + 1, 'revocation rebuilds a persistent Fast query')
  assert.equal(options.at(-1).settings.fastMode, false)

  await setFast(true)
  await turn()
  const activeQuery = s.liveQuery
  s.streaming = true
  const deferredOff = await setFast(false)
  assert.equal(deferredOff.error, undefined)
  assert.equal(deferredOff.result.fastMode, false)
  assert.equal(deferredOff.result.fastModePending, true)
  assert.equal(s.liveQuery, activeQuery, 'changing Fast must not close the active query')
  assert.equal(activeQuery.isClosed, false)
  s.streaming = false
  await setFast(false)
  s.model = 'claude-sonnet-4-6'
  assert.match((await setFast(true)).error.message, /model/)
  assert.equal(buildSessionMeta(s).fastMode, false)
  s.model = 'claude-opus-5-5'
  await setFast(true)
  await call('claude.setModel', { sessionId: 'fast-test', model: 'claude-sonnet-4-6' })
  assert.equal(buildSessionMeta(s).fastMode, false, 'changing to an unsupported model clears Fast')
  assert.equal(events.at(-1).data.meta.fastMode, false, 'model changes broadcast canonical Fast state')
  s.model = 'claude-opus-5-5'
  await setFast(true)
  delete process.env.BAT_DEBUG
  assert.equal(buildSessionMeta(s).fastMode, false, 'debug revocation is checked on metadata and sends')
  await turn()
  assert.equal(options.at(-1).settings.fastMode, false)
  assert.equal((await setFast(false)).result.fastMode, false, 'disabling remains available without debug')
  assert.ok(events.some(e => e.name === 'claude:status' && e.data.meta.fastMode === true))
  assert.equal((await call('claude.setFastMode', { sessionId: 'fast-test', enabled: 'true' })).error !== undefined, true)

  // Hold real sends open across toggles. Late provider status belongs to that
  // query, not the next-turn preference the host has already acknowledged.
  process.env.BAT_DEBUG = '1'
  policy(true, 2)
  const heldTurns = []
  __setSdkOverrideForTests({
    query({ prompt, options: opts }) {
      return (async function* () {
        for await (const _message of prompt) {
          let release
          const wait = new Promise(resolve => { release = resolve })
          heldTurns.push({ opts, release })
          yield { type: 'system', subtype: 'init', session_id: 'held-fast-sdk', model: opts.model,
            fast_mode_state: opts.settings.fastMode ? 'on' : 'off' }
          await wait
          yield { type: 'system', subtype: 'status', fast_mode_state: opts.settings.fastMode ? 'on' : 'off' }
          yield { type: 'result', subtype: 'success', session_id: 'held-fast-sdk', result: 'done', num_turns: 1 }
        }
      })()
    },
  })
  await setFast(false)
  async function startHeldTurn() {
    const count = heldTurns.length
    const reply = await call('claude.sendMessage', { sessionId: 'fast-test', prompt: 'held test only' })
    assert.equal(reply.result.ok, true)
    for (let i = 0; i < 100 && heldTurns.length === count; i++) await new Promise(resolve => setImmediate(resolve))
    assert.equal(heldTurns.length, count + 1)
    assert.equal(s.streaming, true)
    return heldTurns.at(-1)
  }
  async function finishHeldTurn(held) {
    const finished = s.sendQueue
    held.release()
    await finished
    assert.equal(s.streaming, false)
  }

  const standardTurn = await startHeldTurn()
  const standardQuery = s.liveQuery
  assert.equal(standardTurn.opts.settings.fastMode, false)
  assert.equal((await setFast(true)).result.fastModePending, true)
  assert.equal((await setFast(false)).result.fastModePending, false, 'toggling back cancels the pending change')
  assert.equal((await setFast(true)).result.fastModePending, true)
  assert.equal(s.liveQuery, standardQuery)
  assert.equal(standardQuery.isClosed, false)
  assert.equal(standardTurn.opts.abortController.signal.aborted, false)
  await finishHeldTurn(standardTurn)
  assert.equal(buildSessionMeta(s).fastMode, true, 'late off status must preserve the queued opt-in')
  assert.equal(buildSessionMeta(s).fastModePending, true)

  const fastTurn = await startHeldTurn()
  assert.equal(fastTurn.opts.settings.fastMode, true)
  assert.equal(fastTurn.opts.resume, 'held-fast-sdk', 'rebuilding the query must retain the conversation')
  assert.equal(buildSessionMeta(s).fastModePending, false)
  const fastQuery = s.liveQuery
  assert.equal((await setFast(false)).result.fastModePending, true)
  assert.equal(s.liveQuery, fastQuery)
  assert.equal(fastQuery.isClosed, false)
  await finishHeldTurn(fastTurn)
  assert.equal(buildSessionMeta(s).fastMode, false, 'late on status must preserve the queued opt-out')
  assert.equal(buildSessionMeta(s).fastModePending, true)
  const afterFast = await startHeldTurn()
  assert.equal(afterFast.opts.settings.fastMode, false)
  assert.equal(buildSessionMeta(s).fastModePending, false)
  await finishHeldTurn(afterFast)

  // A toggle while SDK loading/startup is in progress must also wait for the
  // next send, even though sdk.query has not been constructed yet.
  closeLiveQuery(s)
  let startupToggle
  let startupToggled = false
  __setSendEventForTests((name, data) => {
    events.push({ name, data })
    if (name === 'claude:status' && data.meta.runtimeStatus === 'starting' && !startupToggled) {
      startupToggled = true
      startupToggle = setFast(true)
    }
  })
  const startingTurn = await startHeldTurn()
  assert.equal((await startupToggle).result.fastModePending, true)
  assert.equal(startingTurn.opts.settings.fastMode, false)
  await finishHeldTurn(startingTurn)
  assert.equal(buildSessionMeta(s).fastMode, true)
  __setSendEventForTests((name, data) => events.push({ name, data }))
  const afterStartup = await startHeldTurn()
  assert.equal(afterStartup.opts.settings.fastMode, true)
  assert.equal(buildSessionMeta(s).fastModePending, false)
  await finishHeldTurn(afterStartup)

  closeLiveQuery(s)
  let startupModelChange
  let modelChanged = false
  __setSendEventForTests((name, data) => {
    events.push({ name, data })
    if (name === 'claude:status' && data.meta.runtimeStatus === 'starting' && !modelChanged) {
      modelChanged = true
      startupModelChange = call('claude.setModel', { sessionId: 'fast-test', model: 'claude-sonnet-4-6' })
    }
  })
  const unsupportedTurn = await startHeldTurn()
  assert.equal((await startupModelChange).result, true)
  assert.equal(unsupportedTurn.opts.model, 'claude-sonnet-4-6[1m]')
  assert.equal(unsupportedTurn.opts.settings.fastMode, false, 'the frozen opt-in must still respect the actual request model')
  await finishHeldTurn(unsupportedTurn)
  console.log('fast mode: debug gate, global gate, session opt-in, revocation, query resume and status passed')
} finally {
  closeLiveQuery(s)
  sessions.delete('fast-test')
  __setSdkOverrideForTests(undefined)
  __setSendEventForTests(undefined)
  for (const [key, value] of Object.entries(savedEnv)) {
    if (value === undefined) delete process.env[key]
    else process.env[key] = value
  }
  rmSync(root, { recursive: true, force: true })
}
