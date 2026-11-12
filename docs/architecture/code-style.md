# Code Style & Engineering Conventions

## 1. "Every File Speaks the Same Language"

In Desk-Remote, readability, auditability, and predictability are primary engineering values. Contributors must not introduce personal idiosyncratic idioms. A developer reading any file in any crate should immediately feel at home with the structure, naming, error handling, and logging.

---

## 2. Universal File & Module Header

Every `.rs` source file MUST begin with a standardized module-level documentation comment (`//!`) explaining:
1. The **Single Responsibility** of the module.
2. The **Scope Boundaries** (what it does and what it deliberately does not do).
3. The **Upstream/Downstream Dependencies** (which central systems it interacts with).

```rust
//! # Desk-Remote Transport - WebRTC Peer Connection Manager
//!
//! ## Responsibility
//! Manages the lifecycle of peer-to-peer WebRTC connections, handling ICE candidate
//! gathering, session negotiation (SDP offer/answer), and state transitions.
//!
//! ## Scope Boundaries
//! - Owns WebRTC data channels and media track bindings.
//! - Does NOT handle protocol framing (delegated to `desk-protocol`).
//! - Does NOT manage raw socket connections directly.
```

---

## 3. Naming Conventions

Follow standard Rust naming conventions strictly:

| Element | Convention | Example |
|---|---|---|
| **Crates** | `kebab-case` with `desk-` prefix | `desk-logger`, `desk-transport` |
| **Modules / Files** | `snake_case` | `peer_connection.rs`, `session_manager.rs` |
| **Types / Structs / Enums** | `PascalCase` | `SessionState`, `DeskConnection` |
| **Traits** | `PascalCase` (descriptive verbs/adjectives) | `PacketCodec`, `SessionLifecycle` |
| **Functions / Methods** | `snake_case` (verb-led action) | `start_capture()`, `handle_incoming_frame()` |
| **Constants / Statics** | `SCREAMING_SNAKE_CASE` | `DEFAULT_HEARTBEAT_INTERVAL_MS` |
| **Error Variants** | `PascalCase` (clear noun/adjective phrase) | `PeerDisconnected`, `InvalidPayloadFormat` |

---

## 4. Error Handling Style

- **No Panics**: Never call `.unwrap()` or `.expect()` in non-test production code. Code must never crash an unmonitored gaming rig.
- **Single Error Type**: All public crate APIs must return `Result<T, DeskError>` (or a domain sub-error defined in `desk-error`).
- **Explicit Propagation**: Use the question mark operator (`?`) for concise error bubbling.
- **Context Preservation**: When wrapping third-party or foreign errors, map them cleanly into `DeskError` variants:

```rust
// CORRECT:
pub fn load_pairing_key(path: &Path) -> Result<KeyPair, DeskError> {
    let data = std::fs::read(path).map_err(|e| DeskError::IoError {
        path: path.to_path_buf(),
        source: e,
    })?;
    KeyPair::from_bytes(&data).map_err(|_| DeskError::InvalidKeyFormat)
}

// FORBIDDEN:
pub fn load_pairing_key(path: &Path) -> KeyPair {
    let data = std::fs::read(path).unwrap(); // BANNED
    KeyPair::from_bytes(&data).expect("invalid key") // BANNED
}
```

---

## 5. Logging Style & Patterns

- **No Console Writes**: `println!`, `eprintln!`, `dbg!` are strictly forbidden. Automated CI gates will fail if they are present.
- **Structured Fields**: Prefer key-value structured fields over string concatenation.
- **Semantic Levels**:
  - `trace!`: High-frequency per-frame or packet-level diagnostic data (disabled in release by default).
  - `debug!`: State transitions, message receipt, connection handshake progression.
  - `info!`: Major lifecycle events (service started, peer connected, session created).
  - `warn!`: Non-fatal recoverable issues (temporary network packet drop, retransmissions).
  - `error!`: Recoverable or unrecoverable operational errors (service spawn failure, authentication rejection).

```rust
// CORRECT:
tracing::info!(
    peer_id = %peer.id(),
    address = %peer.remote_addr(),
    "Peer connected successfully"
);

// FORBIDDEN:
println!("Peer {} connected to {}", peer.id(), peer.remote_addr()); // BANNED
```

- **Sensitive Data & Privacy**: Never log plaintext passwords, session encryption keys, or identifiable personal desktop window titles.

---

## 6. Language & Documentation Standards

- **English Only**: The repository is public under an open-source MIT license. All code, inline comments, docstrings, commit messages, and PR descriptions MUST be written in English.
- **User-Facing Strings**: Any text displayed in the GUI or tray notifications must come from `desk-i18n` (canonical English base `en.json`, translated Turkish `tr.json`). Never hardcode UI text strings in Rust source files.

---

## 7. Formatting & Linting Standard

### `rustfmt.toml`
All code must be formatted using official `rustfmt`. The workspace enforces:
```toml
edition = "2021"
max_width = 100
tab_spaces = 4
newline_style = "Windows"
use_field_init_shorthand = true
use_try_shorthand = true
```

### `clippy` Directives
Every crate must enforce strict clippy rules in its `src/lib.rs` or `src/main.rs`:
```rust
#![deny(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::dbg_macro,
    clippy::unwrap_used,
    clippy::expect_used
)]
#![warn(missing_docs)]
```
