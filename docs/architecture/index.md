# Better Agent Terminal Architecture

This document covers the high-level architecture, process model, Claude Code integration hooks, and workspace session management.

## Multi-Workspace Terminal Architecture

Better Agent Terminal (BAT) is built on Tauri (Rust host + React renderer). The architecture supports multiple isolated workspaces, where each workspace manages its own set of terminal processes and an associated AI agent session.

### Process Model

- **Host (Tauri/Rust):**
  - Manages the lifecycle of PTYs (pseudo-terminals) for all workspaces.
  - Handles the persistent storage of workspaces, sessions, and configurations.
  - Exposes a WebSocket remote server (optional) for remote control and mobile clients.
  - Acts as a bridge between the renderer and the bundled Node sidecar.
- **Renderer (React/Vite):**
  - The UI layer, running inside a Tauri WebView.
  - Submits commands to the host using Tauri IPC.
  - Manages the state and layout of workspaces (e.g., active tab, split terminal views, agent panel).
- **Sidecar (Node):**
  - Used where JavaScript SDK/runtime support is strictly required.
  - Integrates with the `@anthropic-ai/claude-agent-sdk` and `@openai/codex`.
  - Spawns the agent CLI binaries (Claude Code and Codex) as subprocesses.

### Workspace Session Management

- Workspaces are organized by project directories.
- Terminals are powered by `xterm.js` in the renderer and `node-pty` in the host.
- Each workspace can host multiple regular terminals and up to one agent session.
- Agent sessions and configurations are persisted locally per workspace.
- **Detachable Windows:** Individual workspaces can be detached into separate windows. The Rust host tracks window-to-workspace mapping and handles IPC routing to ensure output from background processes reaches the correct window.

## Claude Code Integration Hooks

BAT integrates with Claude Code using a built-in Node sidecar that acts as a bridge.

- The Node sidecar uses the `@anthropic-ai/claude-agent-sdk` to spawn the `claude` CLI binary.
- BAT intercepts Claude Code activity via hooks to present a rich UI (streaming text, tool calls, thinking blocks, statusline metrics) without relying on plain PTY output scraping.
- **Permission Modes:** BAT translates UI permission settings (Default, Accept Edits, Plan Mode, Bypass) into the corresponding hook approvals.
- **Git Worktree:** Agents can be launched in isolated worktrees. BAT manages the worktree lifecycle, ensuring the main working tree remains unmodified by autonomous agents.
