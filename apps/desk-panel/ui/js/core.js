// Shared state, DOM helpers, translation lookup and the Tauri bridge.

const tauri = window.__TAURI__;

/** Calls a Rust command. */
export const invoke = (cmd, args) => tauri.core.invoke(cmd, args);
export const newChannel = () => new tauri.core.Channel();
export const appWindow = () => tauri.window.getCurrentWindow();
export const openUrl = (url) => invoke("plugin:opener|open_url", { url });

export const $ = (selector) => document.querySelector(selector);

export const esc = (value) =>
  String(value).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

/** Application state shared by every screen. */
export const S = {
  screen: "list",
  info: { version: "", demo: false },
  settings: { palette: "ink", mode: "system", language: "auto", list_view: "cards", paired_pcs: [] },
  lang: "en",
  catalog: {},
  previews: [],
  pcs: [],
  listState: "loading",
  filter: "all",
  query: "",
  tailscale: { installed: true, connected: false, state: "Unknown", host: null, address: null, checked: false },
  tsConnecting: false,
  tsFailed: false,
};

/** Looks up a dotted key in the loaded catalog and fills `{param}` placeholders. */
export function t(key, params) {
  let node = S.catalog;
  for (const part of key.split(".")) {
    node = node && typeof node === "object" ? node[part] : undefined;
  }
  let text = typeof node === "string" ? node : key;
  if (params) {
    for (const name of Object.keys(params)) text = text.replaceAll(`{${name}}`, params[name]);
  }
  return text;
}

/** SVG icon from the sprite in index.html. */
export const ic = (name, cls = "") =>
  `<svg class="i ${cls}" aria-hidden="true"><use href="#i-${name}"/></svg>`;

let toastTimer;
export function toast(message) {
  const el = $("#toast");
  el.textContent = message;
  el.classList.add("show");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.remove("show"), 2600);
}

/** Screens register their render function here to avoid import cycles. */
export const screens = {};

export function go(name) {
  S.screen = name;
  render();
}

/** tabs.js repaints the tab strip and pauses hidden sessions after every screen change. */
export const hooks = { afterRender() {} };

export function render() {
  document.title = `Desk-Remote · ${t("nav.pcs")}`;
  screens[S.screen]?.();
  hooks.afterRender();
}

/** Keeps focus on a control after a re-render replaced it. */
export function refocus(selector) {
  const el = document.querySelector(selector);
  if (el) el.focus({ preventScroll: true });
}

export function mountApp(html, className = "app") {
  const app = document.getElementById("app");
  app.className = className;
  app.innerHTML = html;
}

// ---------- the one modal ----------
// Every dialog (connection approval, PC picker, incoming file) goes through openModal(). One host
// (#modal), the page behind is inert, Tab stays inside, Esc and the countdown are handled here.
// Stacked dialogs live in the same host: only the top one is visible, closing it reveals the next.

const COUNTDOWN_TICK_MS = 1000;
const FOCUSABLE = "button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex='-1'])";
const modalStack = [];
let countdownTimer = 0;

const topModal = () => modalStack[modalStack.length - 1];

function syncModals() {
  const host = $("#modal");
  modalStack.forEach((m) => {
    m.root.hidden = m !== topModal();
  });
  $("#app").inert = modalStack.length > 0;
  host.hidden = modalStack.length === 0;
}

function paintCountdown(m) {
  const bar = m.root.querySelector(".cd i");
  const text = m.root.querySelector(".cd-text");
  if (bar) bar.style.width = `${Math.round((m.left / m.countdown.seconds) * 100)}%`;
  if (text) text.textContent = m.countdown.text(m.left);
}

function countdownTick() {
  for (const m of [...modalStack]) {
    if (!m.countdown) continue;
    m.left -= 1;
    if (m.left <= 0) m.countdown.onExpire?.();
    else paintCountdown(m);
  }
  if (!modalStack.some((m) => m.countdown)) {
    clearInterval(countdownTimer);
    countdownTimer = 0;
  }
}

/**
 * Opens (or replaces, same `key`) a dialog.
 * `title`/`body`/`headExtra` are trusted HTML (escape user data at the call site). `buttons`:
 * [{act, label, icon?, cls?, id?, disabled?}] wired through data-act like every other button.
 * `focus` (selector) owns the initial focus; give the safest answer. `countdown`:
 * {seconds, text(left), onExpire}. `onEscape` defaults to closing; `onEnter` runs on Enter in a
 * text/search field. Returns the dialog root element.
 */
export function openModal(opts) {
  closeModal(opts.key, { silent: true });
  const { key, role = "dialog", title, icon = "", headExtra = "", body = "", buttons = [], wide = false, focus, countdown } = opts;
  const id = `mdl-${key}`;
  const btns = buttons
    .map(
      (b) =>
        `<button class="btn ${b.cls ?? ""}" data-act="${b.act}" ${b.id ? `id="${b.id}"` : ""} ${b.disabled ? 'disabled aria-disabled="true"' : ""}>${b.icon ? ic(b.icon) : ""}${b.label}</button>`,
    )
    .join("");
  const cd = countdown
    ? `<div class="cd" aria-hidden="true"><i></i></div><p class="hint cd-text">${countdown.text(countdown.seconds)}</p>`
    : "";
  const root = document.createElement("div");
  root.className = "modal-back";
  root.dataset.modal = key;
  root.innerHTML = `<div class="dialog ${wide ? "wide" : ""}" role="${role}" aria-modal="true" aria-labelledby="${id}-t">
    <div class="dlg-head">${icon ? `<div class="ico">${ic(icon)}</div>` : ""}<div><h2 id="${id}-t">${title}</h2>${headExtra}</div></div>
    <div class="dlg-body">${body}</div>${cd}<div class="dlg-act">${btns}</div></div>`;
  const entry = { ...opts, root, left: countdown?.seconds ?? 0, returnTo: document.activeElement };
  modalStack.push(entry);
  $("#modal").append(root);
  syncModals();
  if (countdown && !countdownTimer) countdownTimer = setInterval(countdownTick, COUNTDOWN_TICK_MS);
  (focus && root.querySelector(focus))?.focus({ preventScroll: true });
  if (!root.contains(document.activeElement)) root.querySelector(FOCUSABLE)?.focus({ preventScroll: true });
  return root;
}

/** Closes the dialog with this key and gives focus back to what had it before. */
export function closeModal(key, { silent = false } = {}) {
  const at = modalStack.findIndex((m) => m.key === key);
  if (at < 0) return;
  const [m] = modalStack.splice(at, 1);
  m.root.remove();
  syncModals();
  if (silent) return;
  m.onClose?.();
  if (m.returnTo?.isConnected && !m.returnTo.closest("[hidden]")) m.returnTo.focus({ preventScroll: true });
}

export const modalEl = (key) => modalStack.find((m) => m.key === key)?.root ?? null;

export function setModalButton(key, id, enabled) {
  const b = modalEl(key)?.querySelector(`#${id}`);
  if (!b) return;
  b.toggleAttribute("disabled", !enabled);
  b.setAttribute("aria-disabled", String(!enabled));
}

document.addEventListener("keydown", (e) => {
  const m = topModal();
  if (!m) return;
  if (e.key === "Escape") {
    e.preventDefault();
    (m.onEscape ?? (() => closeModal(m.key)))();
  } else if (e.key === "Tab") {
    const items = [...m.root.querySelectorAll(FOCUSABLE)];
    if (!items.length) return;
    const first = items[0];
    const last = items[items.length - 1];
    if (!m.root.contains(document.activeElement)) {
      e.preventDefault();
      first.focus();
    } else if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  } else if (e.key === "Enter" && m.onEnter && /^(text|search)$/.test(e.target.type ?? "")) {
    e.preventDefault();
    m.onEnter();
  }
});
