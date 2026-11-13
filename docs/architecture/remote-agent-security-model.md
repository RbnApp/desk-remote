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
| Trace on every connection | Log line (target `audit`) on every connection; the tray balloon naming the panel only on the panel's first connection (DAPP-119, remembered in `service.json` as `announced_panels`, cleared when the pairing is removed). | `Hub::record_peer_connected`, `engine::serve_viewer`, helper `link.rs` |
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

## Secure desktop: UAC prompt and lock screen (DAPP-118, owner decision 13.11.2026)

There is **no setting** for this: a paired panel always sees and can click the UAC consent prompt and the
lock screen. "Doing everything remotely is the point of the app; confirming UAC on the PC would defeat it."
The protection is the one above: only a paired panel on a private network gets a stream at all. Nothing
was added that widens who may connect.

- The user's tray helper cannot open the secure (Winlogon) desktop. While viewers watch and the user
  helper reports `DesktopLocked` (or Windows reports the session locked), the supervisor starts a second
  copy of the agent in the same session as **SYSTEM**: `DeskRemote-Ajan.exe helper --secure-desktop`
  (`launch::launch_system_helper`; the service's own token moved to the session).
- That helper has no tray, no windows and no file access. It speaks only picture, cursor and input over its
  own pipe `\\.\pipe\DeskRemote-SecureHelper-<session>`, whose DACL is SYSTEM only; the service verifies
  the connecting pid, image path and SID (SYSTEM) before sending it anything.
- It attaches its capture and input threads to the desktop that receives input
  (`desk_capture::attach_input_desktop`) and follows it when it changes.
- The hub hands the viewers over (`Hub::switch_media`): the secure helper gets a clean join, input goes to it
  alone, and the user helper's stale picture is dropped. When the prompt closes the secure helper is released
  and the user helper restarts its capture with a fresh config and keyframe. The viewers' link and
  `Status` stay online throughout; a locked screen no longer reports the PC as unavailable.
- The supervisor ends the secure helper as soon as it is not needed. In `run --console` (portable, user
  token) there is no SYSTEM service, so the secure desktop stays out of reach there.
- Both helpers are the same executable: service and helpers must be the same version.

## Limits

- The agent service is not stopped from the tray; stopping it is an administrator action (`stop` / Services).
- The supervisor restarts a tray that was closed in service mode.
- The IPC and wire formats gained new variants at the end (`Message::Unpair`) or before `Shutdown` (`IpcMessage`); service and helper must be the same version.
