// JSON-RPC fixture only. It imports no agent/provider code and owns no sessions.
import { createInterface } from 'node:readline'
let calls = []
for await (const line of createInterface({ input: process.stdin })) {
  const { id, method, params } = JSON.parse(line)
  let result
  if (method === 'fixture.reset') { calls = []; result = true }
  else if (method === 'fixture.calls') result = calls
  else if (method === 'fixture.exit') result = true
  else { result = { method, params }; calls.push(result) }
  await new Promise(resolve => process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id, result }) + '\n', resolve))
  if (method === 'fixture.exit') process.exit(0)
}
