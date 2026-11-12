// Theme and language: tokens come from Rust (desk-theme), text from Rust (desk-i18n).

import { S, invoke } from "./core.js";

const systemDark = window.matchMedia("(prefers-color-scheme: dark)");

/** Resolves the effective UI language from the stored setting. */
export function effectiveLanguage() {
  const choice = S.settings.language;
  if (choice === "tr" || choice === "en") return choice;
  return navigator.language.toLowerCase().startsWith("tr") ? "tr" : "en";
}

export async function loadCatalog() {
  S.lang = effectiveLanguage();
  S.catalog = await invoke("get_catalog", { locale: S.lang });
  document.documentElement.lang = S.lang;
}

/** Writes a resolved theme's tokens as CSS custom properties on the given element. */
export function paintTokens(element, resolved) {
  for (const [name, value] of Object.entries(resolved.tokens)) {
    element.style.setProperty(`--${name.replaceAll("_", "-")}`, value);
  }
}

export async function applyTheme() {
  const { palette, mode } = S.settings;
  const resolved = await invoke("get_theme", { palette, mode, systemIsDark: systemDark.matches });
  paintTokens(document.documentElement, resolved);
  document.documentElement.dataset.palette = resolved.palette;
  document.documentElement.dataset.mode = resolved.is_dark ? "dark" : "light";
}

export async function loadPreviews() {
  S.previews = await invoke("palette_previews");
}

/** Re-applies the theme when the OS switches between light and dark. */
export function watchSystemTheme() {
  systemDark.addEventListener("change", () => {
    if (S.settings.mode === "system") applyTheme();
  });
}
