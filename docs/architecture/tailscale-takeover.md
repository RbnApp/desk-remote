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
