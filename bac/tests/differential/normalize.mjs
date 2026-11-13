// Field-level rules only. Unknown fields, nulls, array order and fork-only
// objects remain visible. No rule deletes fields or matches arbitrary subtrees.
export const rules = [
  { path: '/clients/*/handshake/headers/sec-websocket-accept', rule: 'validated RFC6455 accept -> <websocket-accept> (transport)' },
  { path: '/clients/*/sent/*/token', rule: 'exact owned token -> <token>; incorrect tokens unchanged' },
  { path: '/clients/*/{sent,received}/*/{contextId,params/contextId,result/contextId}', rule: 'generated context IDs mapped by first occurrence, references preserved' },
  { path: '/clients/*/sent/*/params/{path,parentPath,cwd,options/cwd}', rule: 'exact owned scratch prefix -> <scratch>' },
  { path: '/clients/*/received/*/result (fs:home,git:getRoot)', rule: 'exact owned scratch prefix -> <scratch>' },
  { path: '/clients/*/received/*/result/path (fs:mkdir,fs:delete-path)', rule: 'exact owned scratch prefix -> <scratch>' },
  { path: '/clients/*/received/*/result/{current,parent,entries/*/path} (fs:list-dirs)', rule: 'exact owned scratch prefix -> <scratch>' },
  { path: '/clients/*/received/*/result/*/path (fs:readdir)', rule: 'exact owned scratch prefix -> <scratch>' },
  { path: '/clients/*/received/*/{params, args/0}/elapsedMs (sidecar:metric)', rule: 'numeric duration -> <duration>' },
  { path: '/clients/*/received/*/{params,args/0} (sidecar:stderr)', rule: 'ISO timestamp prefix and elapsedMs=NUMBER only -> placeholders; log wording retained' },
  { path: '/clients/*/received/*/{result,result/meta,params/meta,args/1} (session meta/state, agent:status)', rule: 'ONLY cwd/options.cwd scratch prefix; durationMs, lastDataAt, runtimeStatusStartedAt, lastTurnDurationMs, lastTurnFirstTokenMs numeric times; inputState.epoch mapped as random identity' },
  { path: '/clients/*/received/*/{result/messages/*,params/message,args/1,params/items/*,args/1/*} (session state,agent:message,agent:history)', rule: 'ONLY id generated identity and timestamp numeric time; message content untouched' },
  { path: '/clients/*/received/*/{params/*,args/*} (notification:update)', rule: 'ONLY cwd scratch prefix; id generated identity; timestamp numeric time' },
]
export function normalize(transcript, server) {
  const result = structuredClone(transcript), contexts = new Map(), epochs = new Map(), ids = new Map()
  const identity = (map, value, label) => {
    if (typeof value !== 'string') return value
    if (!map.has(value)) map.set(value, `<${label}-${map.size}>`)
    return map.get(value)
  }
  const path = value => typeof value === 'string' ? value.replaceAll(server.dir, '<scratch>') : value
  const time = (obj, key, label = 'timestamp') => { if (obj && typeof obj[key] === 'number') obj[key] = `<${label}>` }
  const meta = obj => {
    if (!obj || typeof obj !== 'object') return
    if (obj.cwd) obj.cwd = path(obj.cwd)
    if (obj.options?.cwd) obj.options.cwd = path(obj.options.cwd)
    for (const key of ['durationMs', 'lastTurnDurationMs', 'lastTurnFirstTokenMs']) time(obj, key, 'duration')
    for (const key of ['lastDataAt', 'runtimeStatusStartedAt']) time(obj, key)
    if (obj.inputState?.epoch) obj.inputState.epoch = identity(epochs, obj.inputState.epoch, 'input-epoch')
  }
  const message = obj => {
    if (!obj || typeof obj !== 'object') return
    if (obj.id) obj.id = identity(ids, obj.id, 'message')
    time(obj, 'timestamp')
  }
  const log = value => typeof value === 'string' ? value.replace(/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z /, '<timestamp> ').replace(/\belapsedMs=\d+\b/g, 'elapsedMs=<duration>') : value
  for (const client of result.clients) {
    const requests = new Map(client.sent.filter(f => f.type === 'invoke').map(f => [f.id, f.channel]))
    for (const frame of client.received) if (frame.result?.contextId) identity(contexts, frame.result.contextId, 'context')
    for (const [direction, frames] of [['sent', client.sent], ['received', client.received]]) for (const frame of frames) {
      if (direction === 'sent' && frame.token === server.token) frame.token = '<token>'
      for (const container of [frame, frame.params, frame.result]) if (container && Object.hasOwn(container, 'contextId')) container.contextId = identity(contexts, container.contextId, 'context')
      if (direction === 'sent' && frame.params && typeof frame.params === 'object') {
        for (const key of ['path', 'parentPath', 'cwd']) if (key in frame.params) frame.params[key] = path(frame.params[key])
        if (frame.params.options?.cwd) frame.params.options.cwd = path(frame.params.options.cwd)
      }
      if (direction !== 'received') continue
      const channel = requests.get(frame.id)
      if (['fs:home', 'git:getRoot'].includes(channel)) frame.result = path(frame.result)
      if (['fs:mkdir', 'fs:delete-path'].includes(channel) && frame.result?.path) frame.result.path = path(frame.result.path)
      if (channel === 'fs:list-dirs' && frame.result) {
        for (const key of ['current', 'parent']) if (key in frame.result) frame.result[key] = path(frame.result[key])
        for (const entry of frame.result.entries || []) if (entry.path) entry.path = path(entry.path)
      }
      if (channel === 'fs:readdir' && Array.isArray(frame.result)) for (const entry of frame.result) if (entry.path) entry.path = path(entry.path)
      if (['claude:get-session-meta', 'claude:get-session-state'].includes(channel)) {
        meta(frame.result); meta(frame.result?.meta)
        for (const item of frame.result?.messages || []) message(item)
      }
      if (frame.channel === 'sidecar:metric') { time(frame.params, 'elapsedMs', 'duration'); time(frame.args?.[0], 'elapsedMs', 'duration') }
      if (frame.channel === 'sidecar:stderr') { frame.params = log(frame.params); if (frame.args?.length) frame.args[0] = log(frame.args[0]) }
      if (['agent:status', 'claude:status'].includes(frame.channel)) { meta(frame.params?.meta); meta(frame.args?.[1]) }
      if (['agent:message', 'claude:message'].includes(frame.channel)) { message(frame.params?.message); message(frame.args?.[1]) }
      if (['agent:history', 'claude:history'].includes(frame.channel)) {
        for (const item of frame.params?.items || []) message(item)
        for (const item of frame.args?.[1] || []) message(item)
      }
      if (frame.channel === 'notification:update') for (const items of [frame.params, frame.args]) {
        if (Array.isArray(items)) for (const item of items) { message(item); if (item.cwd) item.cwd = path(item.cwd) }
      }
    }
  }
  return result
}
