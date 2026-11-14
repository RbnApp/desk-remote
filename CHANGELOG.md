# Desk-Remote · Release notes

The section that matches `VERSION` becomes the GitHub Release notes (`## X.Y.Z`). Versions are bumped
only when the owner asks for a build. See [release flow](docs/architecture/release.md).

## 0.1.2

**New**
- The session helper is its own program, `DeskRemote-Session.exe`; Task Manager lists it as "Desk-Remote Session Helper" instead of a second "Agent".
- The Connection tab shows devices online and the total number of tailnet devices separately.
- The tray menu has a Settings window (device name, language, sign-in start, service start, auto-update, unattended access, view-only, upload limit, paired panels); changes are accepted only from the signed-in user's session helper.

**Changed**
- The agent program is now `DeskRemote-Agent.exe` (was `DeskRemote-Ajan.exe`); an upgrade carries an existing install over to the new name.
- The tray menu no longer lists the version; it is in Settings and About.
- Each release carries a one-line `.sha256` file per installer so the website can show a checksum.

**Fixed**
- The remote mouse and picture no longer freeze for about a second every few seconds: periodic keyframes (about 470 KB at 1080p) stalled the whole link on a home upload line. The agent now sends a keyframe every 30 seconds instead of every 2, and on demand when a viewer joins or loses a frame.
- The panel's Updates tab installs a ready update: the "Update" button is shown, the installer starts with the Windows permission prompt, and a failed start is shown instead of doing nothing.

## 0.1.1

**New**
- Remote screen now shows and controls the Windows UAC prompt and the lock screen (secure desktop); no setting, the connection no longer drops.
- Task Manager shows every Desk-Remote program with its own name and the Desk-Remote icon (Agent, Tailnet Helper, Relay, panel).

**Fixed**
- The panel no longer slows the whole PC when idle: it leaked a network-discovery service on every scan. Scans now run every 30 s and the interface stops its timers while hidden.
- The "paired panel connected" balloon appears once per connection instead of repeatedly.
- The portable agent starts at sign-in.

## 0.1.0

**New**
- First Windows release: the agent installer (SYSTEM service plus session helper) and the panel installer.
- Remote screen, keyboard and mouse over the owner's private Tailscale network; PCs can be renamed in the panel.
- Send files to a PC from the Windows 11 Explorer right-click menu.
- Turkish and English interface, ink and petrol themes with system, light and dark modes.

**Notes**
- The installers are not code-signed; Windows SmartScreen warns on first run (More info > Run anyway).
