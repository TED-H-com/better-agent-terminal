import { test } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtemp, open, rm, appendFile, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { readArchivePage } from '../src/lib/archive-page.mjs'
import { readCodexHistoryPage, readCodexHead, listCodexSessions } from '../src/handlers/codex.mjs'
import { HISTORY_RESPONSE_MAX_BYTES } from '../src/lib/history-page.mjs'

async function fixture(path, mb, codex) {
  const file = await open(path, 'w')
  let rows = 0, size = 0
  const content = 'x'.repeat(760 * 1024)
  try {
    if (codex) { const meta = JSON.stringify({ type: 'session_meta', payload: { id: 'big-thread', cwd: '/repo' } }) + '\n'; await file.write(meta); size += Buffer.byteLength(meta) }
    while (size < mb * 1024 * 1024) {
      const line = JSON.stringify(codex ? { type: 'event_msg', timestamp: '2026-01-01T00:00:00Z', payload: { type: 'user_message', message: content } } : { id: `row-${rows}`, role: 'user', content }) + '\n'
      await file.write(line)
      size += Buffer.byteLength(line); rows++
    }
  } finally { await file.close() }
  return rows
}

for (const mb of [30, 500]) {
  test(`${mb}MB archives and Codex rollouts use bounded pages and head-only listing`, { skip: mb === 500 && process.env.BAT_TEST_500MB !== '1' }, async () => {
    const dir = await mkdtemp(join(tmpdir(), 'bat-large-pages-'))
    try {
      const archive = join(dir, 'archive.jsonl')
      const total = await fixture(archive, mb, false)
      let offset = 0, count = 0, previous = total
      do {
        const page = await readArchivePage(archive, offset, 120)
        assert.equal(page.total, total)
        assert.ok(page.lines.length > 0)
        assert.ok(page.lines.reduce((n,l) => n + Buffer.byteLength(l),0) < 2 * 1024 * 1024)
        if (offset) assert.ok(page.bytesRead < 2 * 1024 * 1024, 'warm pages must not scan the file')
        const ids = page.lines.map(l => Number(JSON.parse(l).id.slice(4)))
        assert.equal(ids.at(-1), previous - 1)
        previous = ids[0]
        offset += page.lines.length; count += page.lines.length
        if (!page.hasMore) break
      } while (offset < total)
      assert.equal(count, total)
      assert.equal(previous, 0)
      await rm(archive)
      const rollout = join(dir, 'rollout-big-thread.jsonl')
      await fixture(rollout, mb, true)
      const page = await readCodexHistoryPage(rollout, 'session')
      assert.ok(page.bytesRead <= 2 * 1024 * 1024 + 1)
      assert.ok(page.hasMore)
      assert.ok(page.items.length)
      const older = await readCodexHistoryPage(rollout, 'session', page.before)
      assert.ok(older.before < page.before)
      assert.equal(new Set([...page.items,...older.items].map(i => i.id)).size, page.items.length + older.items.length)
      for (const p of [page,older]) {
        const params = { sessionId:'session', items:p.items }
        const frame = { type:'event', channel:'agent:history', params, args:['session',p.items] }
        assert.ok(Buffer.byteLength(JSON.stringify(frame)) < HISTORY_RESPONSE_MAX_BYTES)
      }
      assert.ok(Buffer.byteLength(await readCodexHead(rollout)) <= 64 * 1024)
      const list = await listCodexSessions('/repo',dir)
      assert.equal(list.length,1)
      assert.equal(list[0].sdkSessionId,'big-thread')
      assert.equal(list[0].preview,'(big-thre...)', 'preview after 64KB is not scanned')
    } finally { await rm(dir,{recursive:true,force:true}) }
  })
}

test('archive index invalidates on append/rewrite and preserves nonempty-line offsets', async () => {
  const dir = await mkdtemp(join(tmpdir(),'bat-index-test-'))
  try {
    const path = join(dir,'archive')
    await writeFile(path,'\n{}\n\r\n{"id":1}\n{"id":2}')
    const page = await readArchivePage(path,0,2)
    assert.equal(page.total,3)
    assert.equal(page.hasMore,true)
    assert.deepEqual(page.lines.map(JSON.parse),[{id:1},{id:2}])
    await appendFile(path,'\n{"id":3}\n')
    assert.equal((await readArchivePage(path,0,2)).total,4)
    await writeFile(path,'{"id":4}\n')
    assert.equal((await readArchivePage(path,0,2)).total,1)
    assert.equal((await readArchivePage(path,99,2)).hasMore,false)
  } finally { await rm(dir,{recursive:true,force:true}) }
})
