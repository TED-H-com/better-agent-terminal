import * as assert from 'node:assert/strict'

async function main() {
  let stored: string | null = null
  let failSave = false
  ;(globalThis as any).window = {
    __TAURI_INTERNALS__: {
      invoke: async (command: string, args?: Record<string, unknown>) => {
        if (command === 'settings_load') return stored
        if (command === 'settings_save') {
          if (failSave) throw new Error('Host settings unavailable')
          stored = String(args?.data)
          return true
        }
        if (command === 'debug_is_debug_mode') return false
        throw new Error(`Unexpected command: ${command}`)
      },
    },
    localStorage: { getItem: () => null },
  }
  const { settingsStore } = await import('../renderer/src/stores/settings-store')
  assert.equal(settingsStore.getSettings().autoResumeAfterRateLimit, false)
  for (const value of [undefined, false, 'true', 1]) {
    stored = JSON.stringify({ autoResumeAfterRateLimit: value })
    await settingsStore.load()
    assert.equal(settingsStore.getSettings().autoResumeAfterRateLimit, false)
  }
  failSave = true
  await assert.rejects(settingsStore.setAutoResumeAfterRateLimit(true), /Host settings unavailable/)
  assert.equal(settingsStore.getSettings().autoResumeAfterRateLimit, false, 'a failed host save cannot enable automatic work')
  failSave = false
  await settingsStore.setAutoResumeAfterRateLimit(true)
  assert.equal(JSON.parse(stored!).autoResumeAfterRateLimit, true)
  await settingsStore.load()
  assert.equal(settingsStore.getSettings().autoResumeAfterRateLimit, true)
  await settingsStore.setAutoResumeAfterRateLimit(false)
  assert.equal(JSON.parse(stored!).autoResumeAfterRateLimit, false)
  assert.equal(settingsStore.getSettings().autoResumeAfterRateLimit, false)
  console.log('Rate-limit resume settings: default-off, strict opt-in, host save and reload passed')
}

main().catch(error => { console.error(error); process.exit(1) })
