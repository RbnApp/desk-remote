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

Hand-over outcome (`takeover` in `...\system\tailscale\tailnet-status.json`): `setting_off | not_needed | unverified | took_over | needs_admin | failed`.

## Setting
`take_over_official_tailscale` (service.json, default `true`). `false` leaves the official client alone.
Panel Settings text key (to be wired by i18n): `settings.tailscale.takeover` (+ `settings.tailscale.takeover_hint`).

## Agent sign-in (remote PC, once, from the agent's own tray)
No auth key and no official Tailscale app are involved (DAPP-113). `desk-tailnet-helper listen` runs for as long
as the agent runs; it has no time limit.

1. First start with no saved login: the helper reports `needs_auth` with a login URL (refreshed every 10 minutes
   while unanswered). The tray shows **Sign in to Tailscale** and a balloon says so once.
2. The user clicks it; the URL opens in the browser of the signed-in user. After the login the helper reports
   `listening` and the tray shows `Tailscale: <ip>`.
3. The login lives in `...\system\tailscale` (admin-only). A restart, a reboot or a Windows sign-out/sign-in does
   not ask again.

The service keeps the helper alive (`crates/desk-service-core/src/tailnet_run.rs`): when it exits it is started again
after 1, 2, 4 ... 60 seconds (the delay starts over once it ran for a minute or reached `listening`). While it is
down the tray shows `Tailscale: stopped, restarting...`. Every change reaches the tray at once.

`tailnet-status.json` is rewritten at every change: `{ "state": "connected | needs_login | error | off", "takeover": { "state": ... } }`.
The hand-over of an official client (above) still only happens after `listening` is verified.

Test: `go test -run TestListenSurvivesSignInAndRestartsWithoutAsking` in `apps/desk-tailnet-helper` (testcontrol: no
key, `needs_auth`, still alive after 125 s, login completes, `listening`, helper killed, restart listens without a
new login) and the restart-loop tests in `tailnet_run.rs`.

## Panel node (viewer PC, no Tailscale app)
The panel (`apps/desk-panel`) runs its OWN tailnet node, so a viewer PC needs no Tailscale app.

- **Node**: `desk-tailnet-helper panel` (same Go code and binary as agent mode, `panel.go`). It is the embedded
  helper extracted to `%APPDATA%\DeskRemote\bin\` (`DeskPaths::panel_tailnet_helper_path`), SHA verified. State
  (the login) lives in `%APPDATA%\DeskRemote\tailscale\` (`DeskPaths::panel_tailnet_dir`). Host name:
  `deskremote-panel-<computer>`.
- **Agent host name** (DAPP-114): the service starts its helper with `--hostname <computer name>` (lower case DNS label,
  `desk_service_core::tailnet_host_name`), so the PC is listed under its own name. A node that already exists is
  renamed in place on the next helper start, with no new login. Without the flag the helper falls back to the
  computer name too; no product placeholder is ever shown.
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
