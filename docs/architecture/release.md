# Release Pipeline

Desk-Remote ships through GitHub Releases, the same way Desk Helper does: **every Release is a new update**.
Releases are created only by `.github/workflows/release.yml`; nobody uploads assets by hand.

## 1. Asset-name contract (single source)

This section is the only place that defines the names. The update client (`crates/desk-update`) must take
these constants from the versioning/identity crate, not repeat them (note for the update-client owner).

| Asset | Name | Content |
|-------|------|---------|
| Agent installer | `DeskRemote-Agent-Setup-<VERSION>.exe` | NSIS; registers the SYSTEM service |
| Panel installer | `DeskRemote-Panel-Setup-<VERSION>.exe` | Tauri NSIS; never installs the agent |
| Checksums | `SHA256SUMS.txt` | One line per installer: `<sha256 lowercase hex>␠␠<file name>` |

- `<VERSION>` is the content of the root `VERSION` file, e.g. `0.1.0`. The tag is `v<VERSION>`.
- `SHA256SUMS.txt` is UTF-8 without BOM, LF line endings, exactly two lines (agent, panel), so
  `sha256sum -c SHA256SUMS.txt` works inside the download folder.
- `scripts/build-installers.ps1` writes these three files into `dist/`; the workflow uploads exactly them
  and fails if one is missing or a checksum does not match.
- Changing a name is a breaking change for already installed clients: change this table, the script and
  the update client together.

## 2. Release flow

1. Owner says "build/publish". Only then: raise `VERSION` (and the workspace version, gate 6 keeps them equal).
2. Move the notes to a dated `## X.Y.Z` section in `CHANGELOG.md` (English).
3. Commit and push to `main`.
4. The workflow runs on `windows-latest` because `VERSION` changed. If `v<VERSION>` already exists it stops.
   Otherwise it builds, checks the assets, and creates the Release with notes taken from the matching
   `CHANGELOG.md` section.
5. Manual run: Actions > Release > Run workflow (same skip rule).

## 3. Rollback

- Bad Release, nothing installed yet: delete the Release and the tag `v<VERSION>` on GitHub; the next push
  that touches `VERSION` or the workflow file builds it again.
- Bad Release, already offered to clients: do not reuse the version. Fix, raise the version (next patch)
  and release again; clients move forward to the newer Release.
- Update-client side: it must refuse an asset whose SHA-256 differs from `SHA256SUMS.txt`.

## 4. Signing

The installers are **not code-signed** and no certificate is bought. Windows SmartScreen warns on first
run ("More info" > "Run anyway"). The Explorer menu sparse package is signed with a locally created
self-signed certificate (see `scripts/build-installers.ps1`); that is not a publisher signature. No signing
secret exists in the workflow; if one is ever added it is referenced only by its GitHub Secret name.
