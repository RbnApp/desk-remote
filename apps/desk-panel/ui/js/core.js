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

export function render() {
  document.title = `Desk-Remote · ${t("nav.pcs")}`;
  screens[S.screen]?.();
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
