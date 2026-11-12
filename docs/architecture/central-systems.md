# Central Systems & Single-Point Contracts

## 1. The Core Architectural Axiom

> *"Duplicate code must never be written; parts of the system that share functionality must consume it from a single source so maintenance cost remains low. Every file must speak the same language. For example, the logger must reside in one central location and determine the logging rules; rogue console/println statements must never be written across different files. The logger is just one example — systems like connection handling must also be centralized; connection code must never be duplicated in multiple places."*  
> — **Owner's Directive**

Any subsystem that touches OS resources, networking, configuration, or identity must have a **single owner crate**. All other crates and applications are consumers only. Re-implementing, circumventing, or duplicating these systems is strictly forbidden.

---

## 2. Central Systems Specification Table

| # | System Name | Owner Crate | Primary Responsibility | Strict Ban (Forbidden Outside Owner) |
|---|-------------|-------------|------------------------|---------------------------------------|
| 1 | **Logging** | `crates/desk-logger` | Structured logging, formatting, rotation, sinks | Direct `println!`, `eprintln!`, `dbg!`, raw file logging |
| 2 | **Configuration** | `crates/desk-config` | Schema, validation, atomic read/write, defaults | Ad-hoc JSON/TOML parsing, rogue config files |
| 3 | **Paths** | `crates/desk-paths` | Canonical OS paths (ProgramData, AppData, logs, pipes) | Hardcoded path strings, direct `std::env::temp_dir` |
| 4 | **Errors** | `crates/desk-error` | Unified error enum, error codes, standard conversion | Ad-hoc String errors, bare unmapped `anyhow` in public APIs |
| 5 | **Transport & Connection** | `crates/desk-transport` | WebRTC P2P, signaling client, TURN relay, LAN direct | Creating raw sockets, HTTP clients, or WebRTC sessions |
| 6 | **Protocol & Messages** | `crates/desk-protocol` | Message schemas, framing, serialization, negotiation | Custom binary framing, manual JSON parsing in apps |
| 7 | **Crypto & Identity** | `crates/desk-crypto` | Ed25519/X25519 keys, DPAPI/Credential Vault, cipher | Plaintext credentials, custom crypto implementations |
| 8 | **IPC** | `crates/desk-ipc` | Windows Named Pipes with strict DACL security | Ad-hoc localhost TCP/UDP sockets, unsynchronized shared memory |
| 9 | **Localization (i18n)** | `crates/desk-i18n` | Catalogs, key lookup, locale fallback (en -> tr) | Hardcoded user-facing strings in UI code |
| 10 | **Version** | `crates/desk-version` | Root `VERSION` single-source integration, build hash | Hardcoded version numbers across crates |
| 11 | **Service & Process Lifecycle** | `crates/desk-service-core`| Windows Service dispatcher, session helper spawning | Spawning unmanaged child processes, manual `taskkill` |
| 12 | **Theme** | `crates/desk-theme` (tokens: `design/tokens/themes.json`) | Palette/mode token resolution, light/dark/system | Hardcoded colors (hex/rgb) in UI code, ad-hoc palettes, per-file theme logic |
| 13 | **File transfer** | `crates/desk-transfer` | File names, `.partial` + SHA-256 receive, send with limit check, free-space check | Writing received files or checking upload limits anywhere else |

---

File transfer (row 13) is one rule set for every receiver: the panel's own host asks its user first, the unattended agent hands the file to the session helper, and both use `desk_transfer::Receiver`. The only limit is `max_upload_mb` (default 100, 1..=1024) in `desk-config`.

---

## 3. Detailed System Specifications & Contracts

### 3.1. Logging System (`crates/desk-logger`)
- **Owner**: `crates/desk-logger`
- **Core Technology**: `tracing`, `tracing-subscriber`, `tracing-appender`.
- **Public API Principle**:
  - Exposes `desk_logger::init(config: &LoggerConfig) -> Result<(), DeskError>`.
  - Consumers use standard structured tracing macros: `tracing::info!`, `tracing::warn!`, `tracing::error!`, `tracing::debug!`, `tracing::trace!`.
  - Determines log rotation (daily/size-capped), file locations (via `desk-paths`), formatting (compact JSON for machine ingestion or human-readable console in debug mode).
- **Strictly Forbidden Outside Owner**:
  - `println!`, `eprintln!`, `dbg!`, and `std::io::stdout/stderr` writes are strictly banned in release code.
  - No application or crate may create its own log files or logging subscribers.
  - Automated clippy gates reject any use of print macros.

### 3.2. Configuration & Settings (`crates/desk-config`)
- **Owner**: `crates/desk-config`
- **Core Technology**: `serde`, `toml` / `serde_json`, atomic file write via temporary swap.
- **Public API Principle**:
  - Exposes strongly typed configuration structures (`ServiceConfig`, `PanelConfig`, `RelayConfig`).
  - Provides `ConfigManager::load()`, `ConfigManager::save()`, and `ConfigManager::update()`.
  - Validates configuration boundaries (port ranges, timeout thresholds, log level strings) upon loading. Returns typed errors on invalid fields.
  - Automatically migrates older config formats to current schema versions.
- **Strictly Forbidden Outside Owner**:
  - Manual file reading/writing of config files in apps.
  - Hardcoded default configurations scattered across disparate source files.

### 3.3. Paths Management (`crates/desk-paths`)
- **Owner**: `crates/desk-paths`
- **Core Technology**: Windows Known Folder APIs (`SHGetKnownFolderPath`), canonical Windows path formatting.
- **Public API Principle**:
  - Provides canonical functions:
    - `desk_paths::program_data_dir() -> PathBuf` (`%ProgramData%\DeskRemote`)
    - `desk_paths::app_data_dir() -> PathBuf` (`%LOCALAPPDATA%\DeskRemote`)
    - `desk_paths::log_dir() -> PathBuf`
    - `desk_paths::config_file_path() -> PathBuf`
    - `desk_paths::pipe_name(name: &str) -> String` (`\\.\pipe\DeskRemote-<name>`)
- **Strictly Forbidden Outside Owner**:
  - Hardcoded path strings such as `"C:\\ProgramData\\..."` or relative path assumptions.
  - Directly querying Windows environment variables (`std::env::var("APPDATA")`) outside `desk-paths`.

### 3.4. Errors & Diagnostics (`crates/desk-error`)
- **Owner**: `crates/desk-error`
- **Core Technology**: `thiserror`, stable error code enums.
- **Public API Principle**:
  - Defines the root error enum `DeskError`, encompassing domain sub-errors (`ConfigError`, `TransportError`, `IpcError`, `CryptoError`, `CaptureError`, `ServiceError`).
  - Every error variant maps to a unique, stable error code (e.g. `ERR_NET_PEER_DISCONNECTED = 4001`) suitable for telemetry, UI reporting, and diagnostics.
  - Implements `From<std::io::Error>`, `From<serde_json::Error>`, etc., converting foreign system errors into typed `DeskError` variants.
- **Strictly Forbidden Outside Owner**:
  - Creating orphan custom error enums in applications that bypass `DeskError`.
  - Exposing raw string errors (`Err("something failed".into())`).
  - Using `.unwrap()` or `.expect()` in non-test production code.

### 3.5. Transport & Connection (`crates/desk-transport`)
- **Owner**: `crates/desk-transport`
- **Core Technology**: WebRTC (P2P data & media channels), WebSockets/TCP signaling, fallback relay client.
- **Public API Principle**:
  - Exposes a unified connection abstraction: `DeskConnection`, `ConnectionState`, and `TransportManager`.
  - Manages peer handshake, STUN/TURN candidate gathering, NAT traversal, and automatic fallback from P2P to relay server.
  - Supports direct LAN / Tailscale connections without internet routing when peers share an accessible network route.
  - **Phase 0 Dependency**: Signaling protocol details, WebRTC library bindings, and network performance parameters will be calibrated against Phase 0 network tests.
- **Strictly Forbidden Outside Owner**:
  - Opening raw sockets (`TcpStream`, `UdpSocket`, `tokio::net`) or HTTP clients in UI, helper, or service crates.
  - Writing ad-hoc network reconnection loops.

### 3.6. Protocol & Messages (`crates/desk-protocol`)
- **Owner**: `crates/desk-protocol`
- **Core Technology**: Strongly typed binary message frames (`postcard` payload inside the DESK/length/CRC32 frame), `serde`. Byte buffers (video) travel raw.
- **Public API Principle**:
  - Defines the single source of truth for all wire protocol structures:
    - Control messages: Handshake, AuthChallenge, AuthResponse, Heartbeat.
    - Remote control: MouseMove, MouseClick, KeyPress, KeyRelease, ClipboardSync.
    - Media metadata: VideoStreamHeader, FrameMeta, QualityUpdate.
  - Provides frame encoder/decoder (`FrameCodec`) with length-delimited framing and CRC/checksum integrity validation.
- **Strictly Forbidden Outside Owner**:
  - Defining ad-hoc message structs in applications.
  - Manual byte-level bitmasking or slicing in transport consumers.

### 3.7. Crypto & Identity (`crates/desk-crypto`)
- **Owner**: `crates/desk-crypto`
- **Core Technology**: Audited crates only: `snow` (Noise_XX_25519_ChaChaPoly_BLAKE2s sessions), `spake2` (first-time PAKE pairing with the short code), `chacha20poly1305`, `hmac`, OS CSPRNG via `getrandom`; Windows DPAPI (`CryptProtectData`) for at-rest storage of the X25519 static key and the trusted-peer list.
- **Public API Principle**:
  - Manages host identity keypairs (public/private key), deriving permanent Device IDs from the public key.
  - Pairing: SPAKE2 keyed by the one-time code (never sent), key confirmation, static-key exchange, persistent pinning (`TrustStore`). Sessions: Noise_XX with pinned static keys, per-direction keys, replay-rejecting nonces.
  - Manages secure storage of pairing secrets and authorized client certificates in Windows Credential Vault or DPAPI-encrypted files.
- **Strictly Forbidden Outside Owner**:
  - Plaintext password storage or unauthenticated connections.
  - Rolling custom encryption, hashing, or handshake algorithms.

### 3.8. Inter-Process Communication (IPC) (`crates/desk-ipc`)
- **Owner**: `crates/desk-ipc`
- **Core Technology**: Windows Named Pipes (`\\.\pipe\DeskRemote-*`), explicit Windows Security Descriptors (DACL).
- **Public API Principle**:
  - Provides typed client-server communication channels:
    - `IpcServer::bind(pipe_name)`
    - `IpcClient::connect(pipe_name)`
  - Enforces strict Windows security descriptors ensuring only `SYSTEM` and authenticated local administrators can access service control pipes.
  - Coordinates IPC messages between the service (Session 0) and the session helper (User session).
- **Strictly Forbidden Outside Owner**:
  - Using localhost TCP sockets for IPC (vulnerable to local port hijacking by unprivileged user software).
  - Unsynchronized shared memory or arbitrary file polling.

### 3.9. Internationalization & Localization (i18n) (`crates/desk-i18n`)
- **Owner**: `crates/desk-i18n`
- **Core Technology**: Key-based catalog loader, locale resolution.
- **Public API Principle**:
  - Exposes `desk_i18n::t!(key: &str, ...params) -> String`.
  - Source catalogs reside in `locales/en.json` (canonical English base) and `locales/tr.json` (Turkish).
  - Automatically falls back to English if a localized key is missing.
- **Strictly Forbidden Outside Owner**:
  - Hardcoded user-visible strings inside `desk-panel` UI components or notification dialogs.
  - Non-English keys or identifiers.

### 3.10. Version System (`crates/desk-version`)
- **Owner**: `crates/desk-version`
- **Core Technology**: Reads root `VERSION` at compile time via `include_str!`.
- **Public API Principle**:
  - Exposes `desk_version::CURRENT: &str = "0.1.0"`.
  - Exposes `desk_version::version_info() -> VersionInfo` (semantic version, git commit hash, build timestamp, target architecture).
  - Protocol handshake automatically validates compatibility using this central version contract.
- **Strictly Forbidden Outside Owner**:
  - Hardcoding `"0.1.0"` strings in individual crate `Cargo.toml` or source files.
  - Changing the version string without explicit instruction from the project owner.

### 3.11. Process & Windows Service Lifecycle (`crates/desk-service-core`)
- **Owner**: `crates/desk-service-core`
- **Core Technology**: Windows Service API (`advapi32::RegisterServiceCtrlHandlerExW`), Windows Terminal Services API (`wtsapi32`).
- **Public API Principle**:
  - Implements the Windows Service control dispatcher, handling `SERVICE_CONTROL_STOP`, `SHUTDOWN`, and `SESSIONCHANGE`.
  - Monitors active user login/logout events via `WTSRegisterSessionNotification`.
  - Launches the helper (the agent's own executable, `DeskRemote-Ajan.exe helper`) into the active console session token using `CreateProcessAsUserW` with appropriate desktop and window station attributes (`Winsta0\Default`).
  - Monitors helper process liveness and restarts it if it crashes or the user session transitions.
- **Strictly Forbidden Outside Owner**:
  - Invoking `std::process::Command::new("taskkill")` or raw shell commands to kill processes.
  - Spawning interactive helper processes directly from Session 0 without proper session token assignment.

### 3.12. Theme System (`crates/desk-theme`, tokens in `design/tokens/themes.json`)
- **Owner**: `crates/desk-theme` (resolver) and `design/tokens/themes.json` (the single data source). The selected palette and mode are stored by `desk-config`.
- **Core Technology**: `serde_json`, the token file embedded at build time (`include_str!`).
- **Public API Principle**:
  - Exposes `desk_theme::resolve(palette: &str, mode: ThemeMode, system_is_dark: bool) -> Theme`, where `Theme` is a typed set of named color tokens (`background`, `surface`, `text`, `primary`, ...).
  - Palettes are data, not code: adding a palette means adding one block to `themes.json`.
  - UI code reads colors only from `Theme` tokens. See [Theme System](theme.md) for the token list, selection model and the hardcoded-color gate.
- **Strictly Forbidden Outside Owner**:
  - Hex, `rgb()` or named-color literals in UI sources (enforced by the Hardcoded Color Gate in [gates.md](gates.md)).
  - Reading `themes.json` directly, or keeping a second copy of any token value.
  - Storing the palette/mode selection anywhere except `desk-config`.
