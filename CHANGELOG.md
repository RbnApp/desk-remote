# Desk-Remote · Release notes

The section that matches `VERSION` becomes the GitHub Release notes (`## X.Y.Z`). Versions are bumped
only when the owner asks for a build. See [release flow](docs/architecture/release.md).

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
