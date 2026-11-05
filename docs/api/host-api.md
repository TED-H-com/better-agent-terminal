# Host API Reference

The `host-api.ts` module (`renderer/src/host-api.ts`) provides a type-safe adapter between the React renderer and the Tauri/Rust host environment.

## Overview

Renderer code should import `host` from this module instead of accessing `window.batAppAPI` directly. The adapter maps established methods and event listeners to Tauri commands and events.

```typescript
import { host } from '../host-api'
```

## Key Features

- **Environment Resolution:** Automatically resolves the appropriate host environment (Tauri vs. unknown/legacy) via `getHostKind()`.
- **Method Proxying:** Proxies method calls to the underlying Tauri IPC commands using Rust's native capabilities.
- **Compatibility Shim:** Provides a fallback compatibility layer for environments missing native support or running in legacy modes.

## Available APIs
The adapter primarily exposes the following namespaces mirroring backend functionality:
- `claude.*` - Controls Claude Agent lifecycle, settings, and sessions.
- `workspace.*` - Manages Workspace state, configuration, and layout details.
- `profile.*` - Interactions for user profiles and environment variables.
- `settings.*` - App-wide settings.
- `git.*` - Git abstractions for history, diffs, and worktrees.
- `fs.*` - Filesystem actions such as reading, listing directories, and checking paths.
- `pty.*` - Pseudo-terminal lifecycle and process management.
- `image.*` - Methods for dealing with file system images and screenshots.
