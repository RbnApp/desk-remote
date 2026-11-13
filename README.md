# Desk-Remote

Self-hosted remote desktop for Windows 10/11. See and control your own PCs over your private
[Tailscale](https://tailscale.com) network. No cloud relay account, no third-party remote desktop service.

## Features

- Remote screen, keyboard and mouse between your PCs.
- Send files to a PC from the Windows 11 Explorer right-click menu.
- Rename PCs in the panel.
- Built-in updates: the panel and the agent check the latest GitHub Release, verify its SHA-256 and update themselves. Settings > Updates has a manual check and an "update at start" switch; the agent tray menu has "Check for updates".
- English and Turkish interface. Ink and petrol themes, with system, light and dark modes.

## Install

Download both installers from the [latest release](https://github.com/RbnApp/desk-remote/releases/latest):

| Installer | Install it on |
| --- | --- |
| `DeskRemote-Agent-Setup-<version>.exe` | Every PC you want to reach (SYSTEM service plus session helper). |
| `DeskRemote-Panel-Setup-<version>.exe` | The PC you control from. |

Check the files against `SHA256SUMS.txt` from the same release.

The installers are not code-signed. On first run Windows SmartScreen warns: choose **More info > Run anyway**.

## How it works

- `desk-service` runs as a Windows service and starts the session helper and the capture pipeline.
- `desk-panel` is the Tauri app you use to pick a PC and connect.
- `desk-tailnet-helper` is an embedded Go helper that joins your tailnet in user space, so no driver is installed.
- Shared logic (protocol, crypto, transport, config, updates) lives in `crates/`, one owner per concern.

## Build from source

Requirements: Rust (stable), Go (for `apps/desk-tailnet-helper`), Tauri CLI v2, Windows 10/11 64-bit.

```powershell
./scripts/build-installers.ps1
```

The installers and `SHA256SUMS.txt` are written to `dist/`.

## Documentation

- [Folder layout](docs/architecture/folder-layout.md)
- [Central systems](docs/architecture/central-systems.md)
- [Security model](docs/architecture/remote-agent-security-model.md)
- [Release pipeline](docs/architecture/release.md)
- [Automated gates](docs/architecture/gates.md)
- [Changelog](CHANGELOG.md)

## License

[MIT](LICENSE)
