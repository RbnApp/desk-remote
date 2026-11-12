# Theme System

Colors in Desk-Remote come from **one source**: `design/tokens/themes.json`. No other file may contain a color literal. The visual preview of every palette and mode is produced from the same file.

## 1. Single Source & Location

- **File**: `design/tokens/themes.json` (platform-independent data; the future mobile client reads the same file).
- **Why `design/tokens/`**: the token file is neither code (`crates/`, `apps/`) nor a translation catalog (`locales/`) nor prose (`docs/`). A dedicated top-level `design/` folder keeps it out of source trees, and `tokens/` leaves room for future non-color tokens (spacing, radius) without a new folder decision. `folder-layout.md` must list `design/` when this document is merged.
- **Resolver**: `crates/desk-theme` embeds the file at build time and is the only code that reads it (see [Central Systems](central-systems.md), system 12).

## 2. File Structure

```json
{
  "schema": 1,
  "defaults": { "palette": "ink", "mode": "system" },
  "modes": ["system", "light", "dark"],
  "palettes": {
    "ink":    { "label": { "en": "Ink", "tr": "Mürekkep" }, "light": { "<token>": "#rrggbb" }, "dark": { } },
    "petrol": { "label": { "en": "Petrol", "tr": "Petrol" }, "light": { }, "dark": { } }
  }
}
```

- Each palette has a `label` (en/tr, shown in settings) and exactly two token blocks: `light` and `dark`.
- `system` is not a token block. It is resolved at runtime to `light` or `dark` from the OS setting.
- All values are 6-digit lowercase hex (`#rrggbb`).

## 3. Tokens (27 per palette per mode)

| Group | Tokens |
|-------|--------|
| Surfaces | `background`, `surface`, `surface-alt`, `surface-hover`, `surface-pressed` |
| Lines | `border`, `focus-ring` |
| Text | `text`, `text-muted` |
| Brand / action | `primary`, `primary-hover`, `primary-pressed`, `on-primary`, `accent` |
| Status (foreground) | `success`, `warning`, `danger`, `info`, `offline` |
| Status (tint background) | `success-bg`, `warning-bg`, `danger-bg`, `info-bg`, `offline-bg` |
| Disabled | `disabled-bg`, `disabled-text`, `disabled-border` |

Usage rules:
- Status foreground goes on its own tint (`success` on `success-bg`) or on `background`/`surface`. Never use color alone to convey status: always pair it with a text label (the preview uses a dot plus label).
- `on-primary` is the only color allowed on `primary`, `primary-hover` and `primary-pressed`.
- `disabled-*` is used for any non-interactive control; `disabled-text` meets 3:1 on `disabled-bg` (WCAG exempts disabled controls, the bar is kept for legibility).
- There is no blue primary. `info` is a status color only.

## 4. Contrast Contract (WCAG 2.1 AA)

Every palette/mode block must satisfy, and the verification script checks:

| Pair | Minimum |
|------|---------|
| `text`, `text-muted` on `background`, `surface`, `surface-alt`, `surface-hover`, `surface-pressed` | 4.5:1 |
| `on-primary` on `primary`, `primary-hover`, `primary-pressed` | 4.5:1 |
| each status foreground on its `-bg` tint | 4.5:1 |
| `primary` on `surface`, `focus-ring` on `background` and `surface` | 3:1 |
| `disabled-text` on `disabled-bg` | 3:1 |

A new palette is accepted only when all pairs pass for both modes.

## 5. Selection Model

| Setting | Values | Default |
|---------|--------|---------|
| `palette` | any key under `palettes` (`ink`, `petrol`) | `ink` |
| `mode` | `system`, `light`, `dark` | `system` |

- Both settings are fields of the panel config in `crates/desk-config/src/model.rs` (`PanelConfig`). `desk-config` owns validation: an unknown `palette` falls back to the default and logs a warning through `desk-logger`; an unknown `mode` falls back to `system`.
- The settings screen lists palettes by iterating `palettes` and showing `label` (localized through `desk-i18n` fallback: tr when the locale is Turkish, en otherwise). The mode control is a three-option radio group: System, Light, Dark.
- The change applies immediately, without restart, and is saved through `ConfigManager::update()`.
- `system` follows the OS app-theme setting and updates live when it changes.
- **Adding a palette** = adding one block to `themes.json`. No Rust, settings-UI or doc change is needed beyond the contrast check.

## 6. Consumption Rules

1. Panel code obtains colors only from `desk_theme::resolve(palette, mode, system_is_dark)` and uses the returned token names.
2. No color literal (`#rgb`, `#rrggbb`, `#rrggbbaa`, `rgb(`, `rgba(`, `hsl(`, `hsla(`) anywhere in `apps/` or `crates/`, except `crates/desk-theme` and its tests.
3. No second copy of a token value, no per-component "slightly different" shade. Need a new shade? Add a token to `themes.json` for all palettes and modes.
4. Previews and docs derive from `themes.json`; they never carry independent values.

## 7. Enforcement

The **Hardcoded Color Gate** in [gates.md](gates.md) (section 6) scans UI sources with the pattern `#[0-9a-fA-F]{3,8}\b|\b(rgba?|hsla?)\(` and fails the build on any match outside `crates/desk-theme` and `spikes/`. `themes.json` is data and is not scanned. A deliberate exception needs a trailing `// color-gate:allow <reason>` comment and review.

A second check (to be added with `crates/desk-theme`) is a unit test that loads `themes.json` and asserts the token set is identical across all palettes and modes and the contract in section 4 holds.

## 8. Platform

The token file and selection model are platform-independent. The Windows panel is the first consumer; `system` mode maps to the Windows app-theme setting (`AppsUseLightTheme`). A mobile client would map it to the platform's equivalent.
