// Reuse the real sidecar dispatcher and its existing in-process SDK injection
// seam (node-sidecar/tests/fast-mode.test.mjs). No provider executable is used.
import { setTimeout as delay } from 'node:timers/promises'
import { createInterface } from 'node:readline'
import { pathToFileURL } from 'node:url'
const server = await import(pathToFileURL(process.env.DIF_SIDECAR_SOURCE))
const { writeMessage, sendEvent } = await import(new URL('./lib/protocol.mjs', pathToFileURL(process.env.DIF_SIDECAR_SOURCE)))
// Put fixture diagnostics on the same JSON-line pipe as runtime events, so
// independent OS stdout/stderr reader threads cannot reorder the fixture.
process.stderr.write = chunk => { sendEvent('sidecar:stderr', String(chunk).trimEnd()); return true }
server.__setSdkOverrideForTests({
  query({ prompt, options }) {
    return (async function* () {
      for await (const _message of prompt) {
        await delay(100)
        yield { type: 'system', subtype: 'init', session_id: 'fixture-sdk', model: options.model,
          tools: [], mcp_servers: [], slash_commands: [] }
        await delay(100)
        yield { type: 'result', subtype: 'success', session_id: 'fixture-sdk', result: 'fixture answer', num_turns: 1 }
      }
    })()
  },
})
server.initLogger()
const lines = createInterface({ input: process.stdin, crlfDelay: Infinity })
lines.on('line', async line => {
  try {
    const reply = await server.dispatch(JSON.parse(line))
    if (reply) writeMessage(reply)
  } catch (err) { process.stderr.write(`${err}\n`); process.exitCode = 1; lines.close() }
})
lines.on('close', () => process.exit(process.exitCode || 0))
