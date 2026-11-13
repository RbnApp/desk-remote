// Settings screen: palette, appearance, language and the Connection (Tailscale) section.

import { S, $, esc, ic, invoke, mountApp, openUrl, refocus, render, screens, t, toast } from "./core.js";
import { Seg, Shell } from "./components.js";
import { transferSettings } from "./transfer.js";
import { applyTheme, loadCatalog } from "./theme.js";

const KEYS_URL = "https://login.tailscale.com/admin/settings/keys";
const POLL_MS = 2000;
const POLL_LIMIT_MS = 300000;
const SIDEBAR_POLL_MS = 20000;

let pollTimer = null;
let farmKey = "";
let tab = "appearance";
const TABS = ["appearance", "transfer", "connection"];

/** Saves a settings patch through Rust (desk-config) and re-applies theme/language. */
export async function saveSettings(patch) {
  S.settings = await invoke("update_settings", {
    palette: patch.palette ?? null,
    mode: patch.mode ?? null,
    language: patch.language ?? null,
    listView: patch.list_view ?? null,
    idleMinutes: patch.idle_minutes ?? null,
  });
  if (patch.language !== undefined) await loadCatalog();
  await applyTheme();
}

function previewSwatch(preview, mode) {
  const resolved = mode === "light" ? preview.light : preview.dark;
  const style = Object.entries(resolved.tokens)
    .map(([k, v]) => `--${k.replaceAll("_", "-")}:${v}`)
    .join(";");
  return `<div class="pv" style="${style}"><div class="bar"></div><div class="ln"></div><div class="dots"><i class="s"></i><i class="w"></i><i class="d"></i><i class="a"></i></div><small>${t(`theme.${mode}`)}</small></div>`;
}

function paletteCard(preview) {
  const label = S.lang === "tr" ? preview.label_tr : preview.label_en;
  return `<label><input type="radio" name="palette" value="${preview.id}" ${S.settings.palette === preview.id ? "checked" : ""}><div class="pc"><div class="pn"><span>${esc(label)}</span><span class="tick">${ic("check")}</span></div><div class="sw2">${previewSwatch(preview, "light")}${previewSwatch(preview, "dark")}</div></div></label>`;
}

const IDLE_CHOICES = [
  { v: 0, k: "off" },
  { v: 15, k: "m15" },
  { v: 30, k: "m30" },
  { v: 60, k: "m60" },
  { v: 120, k: "m120" },
];

function idleSection() {
  const options = IDLE_CHOICES.map((o) => ({ v: o.v, l: t(`set.idle.${o.k}`) }));
  return `<section class="sec"><div><h2>${t("set.idle.title")}</h2><p class="hint">${t("set.idle.hint")}</p></div>${Seg("idle", t("set.idle.label"), options, S.settings.idle_minutes)}</section>`;
}

function tailscaleSection() {
  const ts = S.tailscale;
  const running = ts.state === "running";
  const state = t(`set.tailscale.state.${ts.state}`);
  const badge = running
    ? `<span class="badge online">${ic("dot")}${state}</span>`
    : `<span class="badge offline">${ic("ring")}${state}</span>`;
  const details = running
    ? `<dl class="kv"><dt>${t("set.tailscale.address")}</dt><dd>${esc(ts.address ?? "—")}</dd><dt>${t("set.tailscale.peers")}</dt><dd>${ts.peers}</dd></dl>`
    : "";
  let action;
  if (S.tsConnecting) {
    action = `<div class="msg info" role="status">${ic("arc", "spin")}${t("set.tailscale.connecting")}</div><div class="inline"><button class="btn" data-act="ts-cancel">${ic("x")}${t("set.tailscale.cancel")}</button></div>`;
  } else if (running) {
    action = `<div class="inline"><button class="btn" data-act="ts-disconnect">${ic("power")}${t("set.tailscale.disconnect")}</button></div><p class="hint">${t("set.tailscale.disconnect_hint")}</p>`;
  } else {
    action = `<div class="inline"><button class="btn primary" data-act="ts-connect">${ic("link")}${t("set.tailscale.connect")}</button></div><p class="hint">${t("set.tailscale.connect_hint")}</p>`;
  }
  const failed = S.tsFailed ? `<div class="msg err" role="alert">${ic("alert")}${t("set.tailscale.failed")}</div>` : "";
  return `<section class="sec" id="tsSection"><div><h2>${t("set.tailscale.title")}</h2><p class="hint">${t("set.tailscale.hint")}</p></div>
    <div class="inline"><span class="lbl">${t("set.tailscale.status")}</span>${badge}</div>${details}${action}${failed}
    <div class="divider"></div>${farmKeyArea()}</section>`;
}

const keyCommand = () => `tailscale up --auth-key=${farmKey.trim() || t("set.tailscale.key.ph")}`;

function farmKeyArea() {
  return `<div class="field"><h2 style="font-size:var(--fs-md)">${t("set.tailscale.key.title")}</h2><p class="hint">${t("set.tailscale.key.hint")}</p>
    <div><button class="btn" data-act="ts-keys">${ic("link")}${t("set.tailscale.key.open")}</button></div>
    <label for="farmKey">${t("set.tailscale.key.label")}</label><input id="farmKey" class="input" style="font-family:var(--mono)" type="password" autocomplete="off" spellcheck="false" placeholder="${esc(t("set.tailscale.key.ph"))}" value="${esc(farmKey)}">
    <span class="lbl">${t("set.tailscale.key.command")}</span><div class="cmd" id="keyCmd">${esc(keyCommand())}</div>
    <div><button class="btn" data-act="ts-copy">${ic("check")}${t("set.tailscale.key.copy")}</button></div></div>`;
}

function renderSettings() {
  const head = `<header class="head"><div class="grow"><h1>${t("set.title")}</h1><div class="sub">${t("set.sub")}</div></div></header>`;
  const cards = S.previews.map(paletteCard).join("");
  const modes = [
    { v: "system", l: t("theme.system") },
    { v: "light", l: t("theme.light") },
    { v: "dark", l: t("theme.dark") },
  ];
  const langs = [
    { v: "auto", l: t("set.lang.auto") },
    { v: "tr", l: "Türkçe" },
    { v: "en", l: "English" },
  ];
  const panels = {
    appearance: () => `
    <section class="sec"><div><h2>${t("set.palette.title")}</h2><p class="hint">${t("set.palette.hint")}</p></div><div class="pal" role="radiogroup" aria-label="${t("set.palette.title")}">${cards}</div></section>
    <section class="sec"><div><h2>${t("set.mode.title")}</h2><p class="hint">${t("set.mode.hint")}</p></div>${Seg("mode", t("set.mode.title"), modes, S.settings.mode)}</section>
    <section class="sec"><div><h2>${t("set.lang.title")}</h2><p class="hint">${t("set.lang.hint")}</p></div>${Seg("lang", t("set.lang.title"), langs, S.settings.language)}</section>`,
    transfer: transferSettings,
    connection: () => idleSection() + tailscaleSection(),
  };
  const tabs = TABS.map(
    (id) => `<button role="tab" id="stab-${id}" aria-selected="${id === tab}" aria-controls="spanel" tabindex="${id === tab ? 0 : -1}" data-stab="${id}">${t(`set.tabs.${id}`)}</button>`,
  ).join("");
  const body = `<div class="stabs" role="tablist" aria-label="${esc(t("set.tabs.label"))}">${tabs}</div>
    <div id="spanel" role="tabpanel" aria-labelledby="stab-${tab}">${panels[tab]()}</div>`;
  mountApp(Shell(body, "settings", head));
}

/** Handles palette/mode/language radio changes. */
export async function onSettingChange(name, value) {
  const field = { palette: "palette", mode: "mode", lang: "language" }[name];
  await saveSettings({ [field]: value });
  render();
  refocus(`input[name="${name}"]:checked`);
  toast(t("set.saved"));
}

/** Saves the idle auto-close time (minutes; 0 = off). */
export async function onIdleChange(value) {
  await saveSettings({ idle_minutes: Number(value) });
  render();
  refocus('input[name="idle"]:checked');
  toast(t("set.saved"));
}

export function onFarmKeyInput(value) {
  farmKey = value;
  const el = $("#keyCmd");
  if (el) el.textContent = keyCommand();
}

export async function copyKeyCommand() {
  try {
    await navigator.clipboard.writeText(keyCommand());
    toast(t("set.tailscale.key.copied"));
  } catch {
    toast(t("set.tailscale.failed"));
  }
}

export const openKeysPage = () => openUrl(KEYS_URL);

export async function refreshTailscale() {
  try {
    const status = await invoke("tailscale_status");
    S.tailscale = { ...status, checked: true };
  } catch {
    S.tailscale = { ...S.tailscale, checked: true, state: "failed" };
  }
  if (S.screen === "settings" && !$("#farmKey:focus")) render();
  else if (S.screen === "list" || S.screen === "pair") render();
}

/** Signs the panel node out of the tailnet (an official Tailscale client is never touched). */
export async function disconnectTailscale() {
  S.tsFailed = false;
  try {
    await invoke("tailscale_disconnect");
  } catch {
    S.tsFailed = true;
  }
  await refreshTailscale();
  render();
}

/** Starts the panel node's browser sign-in and polls until it reports running. */
export async function connectTailscale() {
  S.tsConnecting = true;
  S.tsFailed = false;
  render();
  try {
    await invoke("tailscale_connect");
  } catch {
    S.tsConnecting = false;
    S.tsFailed = true;
    render();
    return;
  }
  const started = Date.now();
  clearInterval(pollTimer);
  pollTimer = setInterval(async () => {
    await refreshTailscale();
    if (S.tailscale.state === "running" || Date.now() - started > POLL_LIMIT_MS) {
      clearInterval(pollTimer);
      S.tsConnecting = false;
      render();
    }
  }, POLL_MS);
}

/** Stops waiting for the browser sign-in (the node itself keeps its state). */
export function cancelTailscaleConnect() {
  clearInterval(pollTimer);
  S.tsConnecting = false;
  refreshTailscale();
  render();
}

export function startSidebarPolling() {
  refreshTailscale();
  setInterval(() => {
    if (!S.tsConnecting && S.screen !== "viewer") refreshTailscale();
  }, SIDEBAR_POLL_MS);
}

screens.settings = renderSettings;

function selectTab(id, focus) {
  tab = id;
  render();
  if (focus) $(`#stab-${id}`)?.focus();
}

document.addEventListener("click", (e) => {
  const el = e.target.closest?.("[data-stab]");
  if (el) selectTab(el.dataset.stab, false);
});

document.addEventListener("keydown", (e) => {
  const el = e.target.closest?.("[data-stab]");
  const step = { ArrowRight: 1, ArrowLeft: -1 }[e.key];
  if (!el || (!step && e.key !== "Home" && e.key !== "End")) return;
  e.preventDefault();
  const i = e.key === "Home" ? 0 : e.key === "End" ? TABS.length - 1 : (TABS.indexOf(el.dataset.stab) + step + TABS.length) % TABS.length;
  selectTab(TABS[i], true);
});
