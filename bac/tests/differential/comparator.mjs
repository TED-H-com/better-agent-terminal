import { isDeepStrictEqual } from 'node:util'
import { createHash } from 'node:crypto'

export const missing = { $missing: true }
const escape = key => String(key).replaceAll('~', '~0').replaceAll('/', '~1')
export function canonical(value) {
  if (Array.isArray(value)) return value.map(canonical)
  if (value && typeof value === 'object') return Object.fromEntries(Object.keys(value).sort().map(k => [k, canonical(value[k])]))
  return value
}
export function differences(left, right, path = '') {
  if (isDeepStrictEqual(left, right)) return []
  if (left === missing || right === missing) return [{ path, upstream: left, fork: right }]
  if (left && right && typeof left === 'object' && typeof right === 'object' && Array.isArray(left) === Array.isArray(right)) {
    return [...new Set([...Object.keys(left), ...Object.keys(right)])].flatMap(key => differences(
      Object.hasOwn(left, key) ? left[key] : missing,
      Object.hasOwn(right, key) ? right[key] : missing, `${path}/${escape(key)}`))
  }
  return [{ path, upstream: left, fork: right }]
}
function matches(actual, expected) {
  if (expected && typeof expected === 'object' && Object.keys(expected).length === 1 && expected.$string) {
    return typeof actual === 'string' && Object.keys(expected.$string).sort().join(',') === 'length,sha256'
      && actual.length === expected.$string.length
      && createHash('sha256').update(actual).digest('hex') === expected.$string.sha256
  }
  if (actual && expected && typeof actual === 'object' && typeof expected === 'object') {
    return Array.isArray(actual) === Array.isArray(expected)
      && isDeepStrictEqual(Object.keys(actual).sort(), Object.keys(expected).sort())
      && Object.keys(expected).every(key => matches(actual[key], expected[key]))
  }
  return isDeepStrictEqual(actual, expected)
}
// No regex or subtree suppression in the allowlist: exact field path and exact
// values (large strings may use length + SHA-256) (including an explicit missing marker). Entries must each be exercised.
export function compare(left, right, allowed = []) {
  if (new Set(allowed.map(e => e.id)).size !== allowed.length) throw new Error('Duplicate allowlist ids')
  const found = differences(canonical(left), canonical(right))
  const used = new Set()
  for (const difference of found) {
    const matching = allowed.filter(entry => entry.path === difference.path
      && matches(difference.upstream, entry.upstream)
      && matches(difference.fork, entry.fork))
    if (matching.length > 1) throw new Error(`Overlapping allowlist: ${difference.path}`)
    if (matching.length) {
      if (!matching[0].id || !matching[0].reason) throw new Error('Allowlist entries need id and reason')
      difference.allowedBy = matching[0].id
      used.add(matching[0].id)
    }
  }
  const stale = allowed.filter(entry => !used.has(entry.id)).map(entry => entry.id)
  return { ok: found.every(d => d.allowedBy) && !stale.length, differences: found, stale, used: [...used] }
}
