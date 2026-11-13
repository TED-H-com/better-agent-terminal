export type GeneratedImage = { dataUrl?: string; path?: string; revisedPrompt?: string }

// Rollouts may store file: URLs while live events carry native paths. Decode
// only URLs, preserving literal percent signs in ordinary filesystem paths.
function imagePath(path: string): string {
  if (!/^file:\/\//i.test(path)) return path
  try {
    const url = new URL(path)
    const pathname = decodeURIComponent(url.pathname)
    if (url.hostname && url.hostname !== 'localhost') return `//${url.hostname}${pathname}`
    if (/^\/[a-z]:\//i.test(pathname)) return pathname.slice(1).replace(/\//g, '\\')
    return pathname
  } catch {
    return path
  }
}

export function parseGeneratedImageResult(result: unknown): GeneratedImage | null {
  let value = result
  if (typeof value === 'string') {
    try { value = JSON.parse(value) } catch { return null }
  }
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null
  const parsed = value as { type?: string; dataUrl?: string; path?: string; revisedPrompt?: string }
  if (parsed.type !== 'image_generation' && parsed.type !== 'image_view') return null
  if (typeof parsed.dataUrl === 'string' && parsed.dataUrl.startsWith('data:image/')) {
    return { dataUrl: parsed.dataUrl, revisedPrompt: parsed.revisedPrompt }
  }
  if (typeof parsed.path === 'string' && parsed.path) {
    return { path: imagePath(parsed.path), revisedPrompt: parsed.revisedPrompt }
  }
  return null
}

export function resolveToolImage(
  toolName: string,
  input: Record<string, unknown>,
  result: unknown,
  status: string,
): GeneratedImage | null {
  const viewed = toolName === 'image_view' || toolName === 'view_image'
  if (!viewed && toolName !== 'image_gen') return null
  const image = parseGeneratedImageResult(result)
  if (image) return image
  // Older live events and history have no image result at all, but the viewed
  // file is still in the input. Do not preview a failed or unfinished call.
  if (viewed && status === 'completed' && typeof input.path === 'string' && input.path) {
    return { path: imagePath(input.path) }
  }
  return null
}
