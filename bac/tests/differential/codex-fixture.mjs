#!/usr/bin/env node
// Adapt the JSON-line fake in codex_pm_runtime.rs::tests::FAKE to the ordinary
// app-server lifecycle. This program never invokes a provider or executes tools.
import { createInterface } from 'node:readline'
if (process.argv.includes('--version')) { console.log('codex-cli fixture'); process.exit(0) }
if (!process.argv.includes('app-server')) process.exit(97)
const send = value => process.stdout.write(JSON.stringify(value) + '\n')
const event = (method, params) => send({ method, params })
const thread = { id: 'fixture-thread', turns: [] }
const lines = createInterface({ input: process.stdin, crlfDelay: Infinity })
lines.on('line', line => {
  const q = JSON.parse(line), p = q.params || {}
  if (q.method === 'initialized') return
  if (q.method === 'turn/start') {
    send({ id: q.id, result: { turn: { id: 'fixture-turn' } } })
    // Let the host install the response's active-turn identity before events.
    setTimeout(() => {
      const turn = { id: 'fixture-turn', status: 'completed', items: [
        { id: 'fixture-user', type: 'userMessage', content: p.input },
        { id: 'fixture-assistant', type: 'agentMessage', text: 'fixture answer', phase: 'final_answer' },
      ] }
      thread.turns = [turn]
      event('turn/started', { threadId: thread.id, turn: { id: turn.id, status: 'inProgress' } })
      for (const item of turn.items) {
        event('item/started', { threadId: thread.id, turnId: turn.id, item })
        event('item/completed', { threadId: thread.id, turnId: turn.id, item })
      }
      event('turn/completed', { threadId: thread.id, turn })
    }, 80)
    return
  }
  const results = {
    initialize: { userAgent: 'fixture' },
    'thread/start': { thread }, 'thread/read': { thread }, 'thread/resume': { thread },
    'account/read': { account: null, requiresOpenaiAuth: false },
    'model/list': { data: [], nextCursor: null },
    'turn/interrupt': {},
  }
  if (!Object.hasOwn(results, q.method)) {
    send({ id: q.id, error: { code: -32601, message: `Fixture does not implement ${q.method}` } }); return
  }
  send({ id: q.id, result: results[q.method] })
})
lines.on('close', () => process.exit(0))
