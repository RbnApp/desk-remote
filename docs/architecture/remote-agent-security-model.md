# Remote agent security model

Decision K-581181 = A (2026-11-13). Applies to the PC agent (`DeskRemote-Ajan`) and the panel.

## Decision

A paired panel is **permanently allowed** (no per-connection prompt on the PC), but the model is strict:
every connection and every offered file leaves a trace the PC's user can see, either side can end the pairing,
and the agent only serves private networks.

## Rules and where they live

| Rule | Behaviour | Code |
| --- | --- | --- |
| Paired = permanent | A panel paired once with the one-time code reconnects without a code. | `desk-crypto` trust store |
| Trace on every connection | Log line (target `audit`) plus a tray balloon naming the panel. | `Hub::record_peer_activity`, `engine::serve_viewer`, helper `link.rs` |
| Trace on every file | Log line with a sanitised name and size plus a tray balloon. Files still need an authenticated pairing. | `transfer::serve` |
| Remove pairing, PC side | Tray menu "Paired panels" asks for confirmation, forgets the panel, drops its viewers. | helper `menu.rs`, `Hub::unpair_device` |
| Remove pairing, panel side | "Remove pairing" on a PC tells the PC (`Message::Unpair`) when reachable and always forgets it locally. | panel `pc_unpair`, `Backend::unpair` |
| Private networks only | The listener closes connections whose source is not loopback, 10/8, 172.16/12, 192.168/16, 100.64/10 or IPv6 ULA. The bind itself stays on all interfaces. | `desk_transport::is_private_peer` |
| mDNS off by default | `lan_announce` defaults to `false`. A saved `service.json` that already says `true` is kept. | `desk-config` model, `host.rs` |
| Tray "Close" is not "Stop" | Closes the tray; stops only a console agent the same user started. Never touches the Windows service, never asks for UAC. | `elevate::quit_agent` |
| Stop event ACL | Only SYSTEM, Administrators and the creating user may signal the console stop event. | `elevate::stop_event_sddl` |

## Consent hardening (DAPP-97 M3/M4)

- "Disconnect all" now also clears the pending consent request and the active session and tells the panel the session ended.
- "Always allow" is written to the pairing store after the hub state lock is released (documented lock order: state, then pairing).

## Limits

- The agent service is not stopped from the tray; stopping it is an administrator action (`stop` / Services).
- The supervisor restarts a tray that was closed in service mode.
- The IPC and wire formats gained new variants at the end (`Message::Unpair`) or before `Shutdown` (`IpcMessage`); service and helper must be the same version.
