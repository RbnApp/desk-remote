# Folder Layout & Cargo Workspace Discipline

## 1. Principle of Absolute Folder Discipline

Every directory in Desk-Remote has a single, strictly bounded responsibility. Code must exist only where it logically belongs. 

- **No Cross-Contamination**: An application (`apps/*`) must never import code directly from another application. Shared behavior must be extracted into a common crate under `crates/*`.
- **No Rogue Files**: Files must never be created in arbitrary folders, root directories, or ad-hoc subdirectories.
- **Single Responsibility**: Every crate or module serves one primary concern. If a module grows multiple orthogonal responsibilities, it must be decomposed.

---

## 2. Cargo Workspace Structure

Desk-Remote is organized as a Cargo workspace with two primary functional trees: `crates/` (shared libraries) and `apps/` (runnable binaries and background daemons).

```
Desk-Remote/
├── .github/                      # CI workflows, quality gate actions
├── apps/                         # Executable applications and daemons
│   ├── desk-service/             # The agent executable DeskRemote-Ajan.exe: Windows Service (SYSTEM, Session 0), portable double-click mode, `helper` subcommand
│   ├── desk-session-helper/      # Library: user session helper (tray, capture, input), started as `DeskRemote-Ajan.exe helper`
│   ├── desk-panel/               # Desktop management GUI application
│   ├── desk-shell-ext/           # Windows 11 first-level Explorer menu entry (COM DLL desk_remote_menu.dll)
│   └── desk-relay/               # Self-hosted signaling and WebRTC relay server
├── crates/                       # Common shared libraries & single-point systems
│   ├── desk-common/              # Foundational types, primitives, and prelude
│   ├── desk-logger/              # Unified logging & tracing system (CENTRAL)
│   ├── desk-config/              # Typed configuration, validation, storage (CENTRAL)
│   ├── desk-paths/               # Canonical OS paths & directories (CENTRAL)
│   ├── desk-error/               # Centralized error enums and codes (CENTRAL)
│   ├── desk-theme/               # Central theme token reader & resolver (CENTRAL)
│   ├── desk-transport/           # Network transport, WebRTC, P2P & relay (CENTRAL)
│   ├── desk-protocol/            # Binary framing, message schemas, serialization (CENTRAL)
│   ├── desk-crypto/              # Key management, device identity, encryption (CENTRAL)
│   ├── desk-ipc/                 # Named Pipes IPC (service <-> helper <-> panel) (CENTRAL)
│   ├── desk-i18n/                # Localization engine & translation catalogs (CENTRAL)
│   ├── desk-version/             # Build-time and runtime version contract (CENTRAL)
│   ├── desk-exe-identity/        # Task Manager name + icon + version resource of every executable (CENTRAL, DAPP-121)
│   ├── desk-transfer/            # File transfer: receiver (name rules, .partial, SHA) and sender (CENTRAL)
│   ├── desk-service-core/        # Windows service dispatcher & process lifecycle (CENTRAL)
│   ├── desk-capture/             # Screen capture engine (DXGI / WGC) (Phase 1 Wave 2)
│   ├── desk-codec/               # Hardware & software video encoder/decoder (Phase 1 Wave 2)
│   └── desk-input/               # Remote input injection & translation (Phase 1 Wave 2)
├── design/                       # Central design tokens and visual specifications (CENTRAL)
│   └── tokens/                   # Theme token catalogs (themes.json)
├── docs/                         # Project documentation
│   └── architecture/             # Architecture rules, guidelines, gates
├── locales/                      # Translation catalogs (en.json, tr.json)
├── packaging/                    # Installer sources: agent NSIS script, panel NSIS hooks, sparse-package manifest
├── scripts/                      # Build, test, and quality gate automation scripts
├── Cargo.toml                    # Root workspace manifest
├── VERSION                       # Canonical version string (0.1.0)
├── AGENTS.md                     # Core rules & documentation map
└── CLAUDE.md                     # Pointer to AGENTS.md
```

> [!NOTE]
> **Phase 0 Status Update**: Phase 0 spikes for video capture and hardware encoding (DAPP-60 Vera, DAPP-61 Baran) have completed successfully. Dedicated workspace skeletons for screen capture (`crates/desk-capture`), video encoding (`crates/desk-codec`), and remote input (`crates/desk-input`) are established in Phase 1 Wave 1. Full implementations based on spike findings will land in Wave 2.

---

## 3. Directory Responsibilities

### `apps/desk-service`
- **Role**: Windows Service running under the `NT AUTHORITY\SYSTEM` account in Session 0.
- **Responsibility**: Boots automatically at machine startup (even before any user logs in), monitors Windows session changes (`WTSRegisterSessionNotification`), spawns the helper (`DeskRemote-Ajan.exe helper`, same file) into the active console session via `CreateProcessAsUserW`, and provides service watchdog management.
- **Scope Restriction**: No GUI code, no direct desktop screen scraping (Session 0 has no interactive desktop in modern Windows).

### `apps/desk-shell-ext`
- **Role**: In-process COM server (`IExplorerCommand`) that shows "Send with Desk-Remote" in the Windows 11 first-level Explorer menu. Loaded by Explorer through the sparse package `DeskRemote.ExplorerMenu` (`packaging/AppxManifest.xml.in`).
- **Responsibility**: Only builds the menu entry and starts `desk-panel.exe --send <paths>`; the title comes from `locales/` through `desk-i18n`. It never talks to the network.

### `packaging/` and `scripts/build-installers.ps1`
- **Role**: Two installers (`dist/DeskRemote-Panel-Setup-<VERSION>.exe`, `dist/DeskRemote-Agent-Setup-<VERSION>.exe`). The panel installer (Tauri NSIS) never installs the agent; the agent installer registers the service and rolls back on failure. Installer texts are generated from `locales/*.json`. Asset names and the release flow: [release.md](release.md).

### `apps/desk-session-helper`
- **Role**: Library holding the lightweight helper that runs inside the active interactive user desktop session. It has no executable of its own: `apps/desk-service` starts it as the `helper` subcommand, so the agent ships as one file.
- **Responsibility**: Executes screen capture, hardware video encoding, receives input injection events (mouse/keyboard), and communicates with `desk-service` via `desk-ipc`.
- **Scope Restriction**: Minimal memory/CPU footprint to avoid interfering with running games or heavy background workloads.

### `apps/desk-panel`
- **Role**: Desktop GUI client used by the administrator/operator.
- **Responsibility**: Manages device pairing, renders remote screen stream, handles operator input, and lists registered PCs and their online status.

### `apps/desk-relay`
- **Role**: Standalone, lightweight signaling and WebRTC TURN/relay server.
- **Responsibility**: Coordinates peer-to-peer connection handshakes, exchanges ICE candidates, and provides relay fallback when P2P or Tailscale/LAN direct connections are blocked by NAT/firewalls.

### `crates/*`
- **Role**: Modular, single-responsibility shared libraries.
- **Responsibility**: Each crate encapsulates one central system or domain model. They contain zero application-specific UI or deployment logic.

---

## 4. Where to Put New Files

1. **New Business Logic or Shared Utilities**:
   - MUST be placed in an existing relevant crate under `crates/`, or in a newly approved crate under `crates/` if it represents an independent domain concern.
   - Example: A new cryptographic handshake function belongs in `crates/desk-crypto/src/handshake.rs`.
2. **New Message Schemas**:
   - MUST be defined in `crates/desk-protocol/src/`.
3. **New Configuration Fields**:
   - MUST be added to `crates/desk-config/src/model.rs`.
4. **New User-Facing Text**:
   - MUST be added to `locales/en.json` (English canonical source) and `locales/tr.json` (Turkish translation), accessed via `desk-i18n`.
5. **New Documentation**:
   - Architecture standards and policies belong in `docs/architecture/`. Feature or deployment docs belong in `docs/`.

---

## 5. Where NEVER to Put Files

- **NEVER** place utility files (`utils.rs`, `helpers.rs`, `common.rs`) inside an application directory if that functionality could be needed by another component.
- **NEVER** place raw log outputs, crash dumps, or temporary debug files in the repository root or source folders.
- **NEVER** duplicate protocol definitions or error types inside `apps/` instead of importing `desk-protocol` or `desk-error`.
- **NEVER** place third-party binaries or libraries in source trees. All dependencies must be managed through Cargo or documented build scripts.
- **NEVER** commit experiments directly into `apps/` or `crates/`. Experimental prototypes belong strictly in `spikes/<topic>/` until formal promotion.
