# Tailscale: always our own helper

No PC (agent or panel) needs the official Tailscale app. The tailnet path is ALWAYS
`desk-tailnet-helper` (tsnet, userspace, run by the SYSTEM service, state in
`%ProgramData%\DeskRemote\system\tailscale`). `TailscaleDetector` (desk-transport) only
reports an official client; it never selects the transport.

## Hand-over of an official client (decision K-581065 = B)
Code: `crates/desk-service-core/src/takeover.rs`, wired in `tailnet.rs`.

1. Helper starts (`listen`) and joins the tailnet.
2. Verify: the helper reported `listening` with a tailnet IP AND the local bridge port accepts TCP.
3. Only then: record the original start type + running flag (`official-tailscale.json`),
   `sc config Tailscale start= disabled`, `sc stop Tailscale`, close `tailscale-ipn.exe`.
4. Verification fails -> official client is NOT touched; outcome `unverified` in `tailnet-status.json`.
5. `desk-service uninstall` restores the recorded start type and restarts the client if it was running.

Outcome file: `...\system\tailscale\tailnet-status.json` (`setting_off | not_needed | unverified | took_over | failed`).

## Setting
`take_over_official_tailscale` (service.json, default `true`). `false` leaves the official client alone.
Panel Settings text key (to be wired by i18n): `settings.tailscale.takeover` (+ `settings.tailscale.takeover_hint`).

## Farm PC (no screen, no Tailscale app)
1. In the Tailscale admin page create a one-time (One-off) auth key. Never commit or log it.
2. Put it in `%ProgramData%\DeskRemote\system\tailscale\authkey.txt` (admin only), then install/start the service.
3. The helper reads it as `TS_AUTHKEY`, joins, and the file is deleted after a successful hand-over;
   the node login stays in the state folder.

## Panel node (viewer PC, no Tailscale app)
The panel (`apps/desk-panel`) runs its OWN tailnet node, so a viewer PC needs no Tailscale app.

- **Node**: `desk-tailnet-helper panel` (same Go code and binary as agent mode, `panel.go`). It is the embedded
  helper extracted to `%APPDATA%\DeskRemote\bin\` (`DeskPaths::panel_tailnet_helper_path`), SHA verified. State
  (the login) lives in `%APPDATA%\DeskRemote\tailscale\` (`DeskPaths::panel_tailnet_dir`). Host name:
  `deskremote-panel-<computer>`.
- **Outgoing path**: the Rust side (`desk_transport::PanelTailnet`, the only place with network primitives) talks
  to a loopback bridge the helper opens on `127.0.0.1:<random>`. A per-launch random token (env `DESK_BRIDGE_TOKEN`,
  never logged) guards it. `CONNECT <token> <ip:port>` replies `OK`/`ERR ...`, then pipes bytes both ways to a
  tsnet `Dial`; only tailnet addresses (100.64.0.0/10, fd7a:115c:a1e0::/48) are accepted. The Noise session
  (`Session`) runs unchanged on top. LAN and loopback targets keep the plain TCP path (`tailnet::dial` in the panel).
- **Discovery**: the helper prints a JSON line on every change (`running` with own IPs and the peer list,
  `needs_auth` with the login URL). The panel list reads those peers; `tailscale status` is no longer used.
- **Sign-in / sign-out** (Settings): `tailscale_connect` starts the node and opens the login URL;
  `tailscale_disconnect` sends `LOGOUT <token>` (the helper logs out and exits). At start the node comes up on
  its own only when a login already exists, so launching the panel never opens a browser.
- **Conflict behaviour**: the panel never reads, stops or changes an official Tailscale client. Both can run
  at once (the panel node is userspace, no adapter, no ports). There is NO takeover in the panel; takeover is
  agent-only (above).
- **Limits**: the panel node does not listen inbound, so the incoming-connection address list no longer adds
  tailnet IPs. The helper ends with the panel (stdin closes).
- **Test**: `cargo test -p desk-transport panel_node -- --ignored --nocapture` (two nodes on 127.0.0.1 through
  testcontrol: panel node connects to the real agent helper and receives frames) plus the Go tests in
  `apps/desk-tailnet-helper` (`panel_test.go`).
