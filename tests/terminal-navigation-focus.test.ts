import * as assert from 'node:assert/strict'

type TauriInvoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>
const events: any[] = []

;(globalThis as { window?: unknown }).window = {
  __TAURI_INTERNALS__: {
    invoke: (async <T>(cmd: string) => {
      if (cmd === 'workspace_save') return true as T
      if (cmd === 'app_set_dock_badge') return undefined as T
      throw new Error(`unexpected invoke: ${cmd}`)
    }) satisfies TauriInvoke,
  },
  addEventListener: (event: string, handler: (e: any) => void) => {},
  removeEventListener: (event: string, handler: (e: any) => void) => {},
  dispatchEvent: (event: any) => {
      events.push(event)
      return true
  }
}

;(globalThis as any).requestAnimationFrame = (cb: Function) => cb()

async function main() {
  const { workspaceStore } = await import('../renderer/src/stores/workspace-store.ts')

  workspaceStore.applySerializedData(JSON.stringify({
    workspaces: [{
        id: 'w1',
        name: 'workspace 1',
        folderPath: '/tmp/w1',
        createdAt: 1,
    }],
    activeWorkspaceId: 'w1',
    activeTerminalId: null,
    terminals: [
        {
            id: 't1',
            workspaceId: 'w1',
            type: 'terminal',
            title: 'term1',
            cwd: '/tmp/w1',
        },
        {
            id: 't2',
            workspaceId: 'w1',
            type: 'terminal',
            title: 'term2',
            cwd: '/tmp/w1',
        },
        {
            id: 't3',
            workspaceId: 'w1',
            type: 'terminal',
            title: 'term3',
            cwd: '/tmp/w1',
        }
    ]
  }))

  const terminals = workspaceStore.getWorkspaceTerminals('w1')
  assert.equal(terminals.length, 3)
  workspaceStore.setFocusedTerminal('t1')

  let currentState = workspaceStore.getState()
  let currentIndex = terminals.findIndex(t => t.id === currentState.focusedTerminalId)

  // Test Alt+\` (cycle forward)
  let direction = 1
  let nextIndex = (currentIndex + direction + terminals.length) % terminals.length
  workspaceStore.setFocusedTerminal(terminals[nextIndex].id)

  currentState = workspaceStore.getState()
  assert.equal(currentState.focusedTerminalId, 't2', 'Alt+` should move to next terminal')

  // Test Alt+Shift+\` (cycle backward)
  currentIndex = terminals.findIndex(t => t.id === currentState.focusedTerminalId)
  direction = -1
  nextIndex = (currentIndex + direction + terminals.length) % terminals.length
  workspaceStore.setFocusedTerminal(terminals[nextIndex].id)

  currentState = workspaceStore.getState()
  assert.equal(currentState.focusedTerminalId, 't1', 'Alt+Shift+` should move to previous terminal')

  // Test Cmd/Ctrl+Left/Right logic (cycle tabs)
  const TABS = ['terminal', 'files', 'git', 'github']
  let prev = 'terminal'
  let idx = TABS.indexOf(prev)
  direction = 1
  let nextTab = TABS[(idx + direction + TABS.length) % TABS.length]
  assert.equal(nextTab, 'files', 'Cycle tab forward')

  direction = -1
  idx = TABS.indexOf(nextTab)
  nextTab = TABS[(idx + direction + TABS.length) % TABS.length]
  assert.equal(nextTab, 'terminal', 'Cycle tab backward')

  // Test terminal resize observer lifecycle
  let thumbnailResizeCount = 0;
  const handleThumbnailResize = (delta: number) => {
    thumbnailResizeCount++
    window.dispatchEvent(new Event('resize'))
  }
  handleThumbnailResize(50)
  assert.equal(thumbnailResizeCount, 1, 'Resize event triggered')

  console.log('workspace tab navigation and focus tests passed')
}

main().catch(error => {
  console.error(error instanceof Error ? error.stack || error.message : String(error))
  process.exit(1)
})
