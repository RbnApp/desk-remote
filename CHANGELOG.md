# Desk-Remote · Release notes

The section that matches `VERSION` becomes the GitHub Release notes (`## X.Y.Z`). Versions are bumped
only when the owner asks for a build. See [release flow](docs/architecture/release.md).

## 0.1.0

**New**
- First Windows release: the agent installer (SYSTEM service plus session helper) and the panel installer.
- Remote screen, keyboard and mouse over the owner's private Tailscale network; PCs can be renamed in the panel.
- Send files to a PC from the Windows 11 Explorer right-click menu.
- Turkish and English interface, ink and petrol themes with system, light and dark modes.

**Notes**
- The installers are not code-signed; Windows SmartScreen warns on first run (More info > Run anyway).
