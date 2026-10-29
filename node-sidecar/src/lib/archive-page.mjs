import { open } from 'node:fs/promises'
// At most 32 offset indexes; transcript bodies are never cached.
const indexes = new Map()
export async function readArchivePage(path, offset, limit) {
  const file = await open(path, 'r')
  let bytesRead = 0
  try {
    const st = await file.stat()
    let index = indexes.get(path)
    if (!index || index.size !== st.size || index.mtime !== st.mtimeMs) {
      const rows = []
      const buffer = Buffer.alloc(64 * 1024)
      let position = 0, start = 0, nonempty = false
      while (position < st.size) {
        const { bytesRead: n } = await file.read(buffer, 0, Math.min(buffer.length, st.size - position), position)
        if (!n) break
        bytesRead += n
        for (let i = 0; i < n; i++) {
          const b = buffer[i]
          if (b === 10) { if (nonempty) rows.push([start, position + i]); start = position + i + 1; nonempty = false }
          else if (b !== 32 && b !== 9 && b !== 13 && b !== 11 && b !== 12) nonempty = true
        }
        position += n
      }
      if (nonempty) rows.push([start, position])
      index = { size: st.size, mtime: st.mtimeMs, rows }
      if (indexes.size >= 32) indexes.clear()
      indexes.set(path, index)
    }
    const total = index.rows.length
    const end = Math.max(0, total - offset), requestedStart = Math.max(0, end - limit)
    let start = end
    const lines = []
    let budget = 1800 * 1024
    for (let row = end - 1; row >= requestedStart; row--) {
      const [a,b] = index.rows[row]
      if (b - a > budget) {
        if (lines.length) break
        lines.push(JSON.stringify({ id: `archive-gap-${a}-${b}`, role: 'system', content: '[History gap: record exceeds page budget]', historyGap: { sizeBytes: b - a } }))
        start = row
        break
      }
      start = row
      budget -= b - a
      const buffer = Buffer.alloc(b - a)
      let read = 0
      while (read < buffer.length) {
        const { bytesRead: n } = await file.read(buffer, read, buffer.length - read, a + read)
        if (!n) throw new Error('Archive changed while paging')
        read += n
      }
      bytesRead += read
      lines.push(buffer.toString('utf8'))
    }
    lines.reverse()
    return { lines, total, hasMore: start > 0, start, bytesRead }
  } finally { await file.close() }
}
