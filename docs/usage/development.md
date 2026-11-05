# Development Guide

This document outlines the workflow for developing the Better Agent Terminal (BAT). For strict operational guidelines, refer to the `AGENTS.md` and `CLAUDE.md` files at the root of the project.

## Prerequisites
- **Node.js**: v18 or later
- **Rust**: stable toolchain
- **pnpm**: Exactly v10 as pinned in `package.json`

## Environment Setup
```bash
git clone https://github.com/tony1223/better-agent-terminal.git
cd better-agent-terminal
corepack enable
pnpm install --frozen-lockfile
```

## Running the Application

### Development Mode
```bash
pnpm run dev
```
Starts the full Tauri development environment (frontend + backend).

### Renderer Only (Frontend)
```bash
pnpm run dev:renderer
```
Starts Vite for frontend iteration.

## Testing and Verification

- **Type checking**:
  ```bash
  pnpm exec tsc --noEmit --pretty false
  ```
- **Build verification**:
  ```bash
  pnpm run compile
  ```
- **Node sidecar tests**:
  ```bash
  pnpm run test:sidecar
  ```
- **Tauri/Rust backend checks**:
  ```bash
  pnpm run check:tauri-rust
  ```
- **Tauri/Rust unit tests**:
  ```bash
  pnpm run test:tauri-rust
  ```
- **Local packaging verification** (without macOS signing/notarization):
  ```bash
  pnpm run tauri:build:debug
  ```

## Logging
- **Frontend (renderer)**: Use `window.batAppAPI.debug.log(...)` instead of `console.log()`. This routes logs through the Tauri host logger, writing them to disk.
- **Backend (Tauri/Rust)**: Use the project's native Rust logging and debug helpers.

Logs are located at:
- macOS (Fresh install): `~/Library/Application Support/com.tonyq.better-agent-terminal/logs/debug.log`
- Windows: `%APPDATA%\com.tonyq.better-agent-terminal\logs\debug.log`
- Linux: `~/.local/share/com.tonyq.better-agent-terminal/logs/debug.log`

In-app, navigate to **Settings → Open Logs Folder** to open this directory directly.
