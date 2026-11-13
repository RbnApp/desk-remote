// "This PC" screen (accept incoming connections), the approval dialog and the session band.
// Commands/events follow the DAPP-81 contract: incoming_* and incoming://request|session|ended.

import { S, $, appWindow, closeModal, esc, ic, invoke, modalEl, mountApp, openModal, refocus, render, screens, t, toast } from "./core.js";
import { Shell } from "./components.js";

const REQUEST_TIMEOUT_S = 30;
const TICK_MS = 1000;
const ATTENTION_INFORMATIONAL = 2;
const APP_TITLE = "Desk-Remote";

/** Live state of "this PC accepts connections"; `available` is false when the build lacks the commands. */
export const inc = {
  available: true,
  loaded: false,
  enabled: false,
  pc_name: "",
  code: "",
  ttlEnd: 0,
  addresses: [],
  pending: null,
  session: null,
};

const MODAL_KEY = "incoming";

function applyState(state) {
  Object.assign(inc, {
    loaded: true,
    available: true,
    enabled: state.enabled,
    pc_name: state.pc_name,
    code: state.code,
    ttlEnd: Date.now() + state.code_ttl_s * 1000,
    addresses: state.addresses ?? [],
    session: state.session ?? null,
  });
  if (state.pending && !inc.pending) openRequest(state.pending);
  else if (!state.pending && inc.pending) closeRequest();
  paintAll();
}

export async function refreshIncoming() {
  try {
    applyState(await invoke("incoming_get_state"));
  } catch {
    inc.available = false;
    inc.loaded = true;
    paintAll();
  }
}

// ---------- window helpers ----------

export async function surfaceWindow() {
  const win = appWindow();
  try {
    await win.show();
    if (await win.isMinimized()) await win.unminimize();
    await win.setAlwaysOnTop(true);
    await win.requestUserAttention(ATTENTION_INFORMATIONAL);
  } catch {
    // Window chrome is best effort; the dialog is already in the page.
  }
}

export const dropWindowPin = () => appWindow().setAlwaysOnTop(false).catch(() => {});

const setTitle = (watched) =>
  appWindow()
    .setTitle(watched ? `${APP_TITLE} — ● ${t("incoming.band.title")}` : APP_TITLE)
    .catch(() => {});

// ---------- screen ----------

const modeLabel = (mode) => t(mode === "control" ? "incoming.mode.control" : "incoming.mode.view");

function ttlText() {
  const left = Math.max(0, Math.ceil((inc.ttlEnd - Date.now()) / 1000));
  return t("incoming.code.expires", { s: left });
}

function enabledBody() {
  const addresses = inc.addresses.length
    ? `<ul class="addrs">${inc.addresses
        .map(
          (a) =>
            `<li><button class="addr" data-act="inc-copy" data-text="${esc(a)}" data-msg="incoming.addr.copied" title="${esc(t("incoming.addr.hint"))}"><span>${esc(a)}</span>${ic("copy")}<span class="sr">${t("incoming.addr.hint")}</span></button></li>`,
        )
        .join("")}</ul>`
    : `<p class="hint">${t("incoming.addr.none")}</p>`;
  const live = inc.session
    ? `<div class="msg warn" role="status">${ic("eye")}<div><b>${esc(inc.session.peer_name)}</b> — ${modeLabel(inc.session.mode)}. ${t("incoming.live.hint")}</div></div>`
    : "";
  return `${live}
    <section class="sec" aria-labelledby="codeTitle"><div><h2 id="codeTitle">${t("incoming.code.title")}</h2><p class="hint">${t("incoming.code.hint")}</p></div>
      <div class="codebig" id="codeBig" aria-label="${esc(t("incoming.code.title"))}">${esc(inc.code)}</div>
      <div class="hint" id="codeTtl">${ttlText()}</div>
      <div class="inline"><button class="btn" data-act="inc-copy" data-text="${esc(inc.code)}" data-msg="incoming.code.copied">${ic("copy")}${t("incoming.code.copy")}</button><button class="btn" data-act="inc-newcode">${ic("refresh")}${t("incoming.code.new")}</button></div></section>
    <section class="sec" aria-labelledby="addrTitle"><div><h2 id="addrTitle">${t("incoming.addr.title")}</h2><p class="hint">${t("incoming.addr.sub")}</p></div>${addresses}
      <dl class="kv"><dt>${t("incoming.pc_name")}</dt><dd>${esc(inc.pc_name)}</dd></dl></section>`;
}

function demoBox() {
  if (!S.info.demo || !inc.enabled) return "";
  return `<section class="sec"><div><h2>${t("incoming.demo.title")}</h2></div><div class="inline">
    <button class="btn sm" data-act="inc-demo-new">${t("incoming.demo.new")}</button>
    <button class="btn sm" data-act="inc-demo-known">${t("incoming.demo.known")}</button></div></section>`;
}

function renderIncoming() {
  const head = `<header class="head"><div class="grow"><h1>${t("incoming.title")}</h1><div class="sub">${t("incoming.sub")}</div></div></header>`;
  let body;
  if (!inc.loaded) {
    body = `<div class="msg info" role="status">${ic("arc", "spin")}${t("incoming.loading")}</div>`;
  } else if (!inc.available) {
    body = `<div class="msg warn" role="status">${ic("alert")}${t("incoming.unavailable")}</div>`;
  } else {
    const state = inc.enabled ? t("incoming.on") : t("incoming.off");
    body = `<section class="sec"><div class="split"><div><h2>${t("incoming.toggle")}</h2><p class="hint">${inc.enabled ? t("incoming.on_hint") : t("incoming.off_hint")}</p></div>
      <label class="sw"><input type="checkbox" id="incEnabled" role="switch" ${inc.enabled ? "checked" : ""}><span class="tr"></span><span>${state}</span></label></div></section>
      ${inc.enabled ? enabledBody() : ""}${demoBox()}`;
  }
  mountApp(Shell(body, "incoming", head));
}

screens.incoming = renderIncoming;

// ---------- session band ----------

function paintBand() {
  const host = $("#band");
  if (!host) return;
  const session = inc.session;
  document.body.classList.toggle("has-band", Boolean(session));
  setTitle(Boolean(session));
  if (!session) {
    host.innerHTML = "";
    return;
  }
  const control = session.mode === "control";
  const toggle = control
    ? `<button class="btn sm" data-act="inc-mode" data-mode="view">${ic("eye")}${t("incoming.band.take")}</button>`
    : `<button class="btn sm" data-act="inc-mode" data-mode="control">${ic("pointer")}${t("incoming.band.give")}</button>`;
  host.innerHTML = `<div class="band ${control ? "ctl" : "view"}" role="status">${ic(control ? "pointer" : "eye")}
    <span class="bt"><b>${t("incoming.band.title")}</b> — ${esc(session.peer_name)} · <span class="bm">${modeLabel(session.mode)}</span></span>
    <span class="spacer"></span>${toggle}<button class="btn sm danger" data-act="inc-disconnect">${ic("power")}${t("incoming.band.disconnect")}</button></div>`;
}

// ---------- approval dialog ----------

function openRequest(request) {
  inc.pending = request;
  const trust = request.known
    ? `<span class="badge online">${ic("check")}${t("incoming.dialog.known")}</span>`
    : `<span class="badge warning">${ic("alert")}${t("incoming.dialog.unknown")}</span>`;
  openModal({
    key: MODAL_KEY,
    role: "alertdialog",
    icon: "monitor",
    title: t("incoming.dialog.title", { name: esc(request.peer_name) }),
    headExtra: trust,
    body: `<p class="hint">${t("incoming.dialog.body")}</p>
      <dl class="kv"><dt>${t("incoming.dialog.fingerprint")}</dt><dd>${esc(request.fingerprint)}</dd></dl>
      <label class="chk"><input type="checkbox" id="incAlways"><span>${t("incoming.dialog.always")}</span></label>`,
    countdown: { seconds: REQUEST_TIMEOUT_S, text: (s) => t("incoming.dialog.countdown", { s }), onExpire: () => answer("reject") },
    buttons: [
      { act: "inc-reject", id: "incReject", cls: "danger", icon: "x", label: t("incoming.dialog.reject") },
      { act: "inc-view", cls: "primary", icon: "eye", label: t("incoming.dialog.view") },
      { act: "inc-control", icon: "pointer", label: t("incoming.dialog.control") },
    ],
    // The safest answer owns the initial focus: an accidental Enter must not grant access.
    focus: "#incReject",
    onEscape: () => answer("reject"),
    onClose: dropWindowPin,
  });
  surfaceWindow();
}

function closeRequest() {
  inc.pending = null;
  closeModal(MODAL_KEY);
}

function paintAll() {
  paintBand();
  if (S.screen === "incoming") render();
}

async function answer(decision) {
  const request = inc.pending;
  if (!request) return;
  const alwaysAllow = decision !== "reject" && Boolean(modalEl(MODAL_KEY)?.querySelector("#incAlways")?.checked);
  closeRequest();
  try {
    await invoke("incoming_answer", { requestId: request.request_id, decision, alwaysAllow });
  } catch {
    toast(t("error.unknown"));
  }
}

function tick() {
  if (!document.hidden && inc.enabled) {
    const ttl = $("#codeTtl");
    if (ttl) ttl.textContent = ttlText();
    if (Date.now() >= inc.ttlEnd) refreshIncoming();
  }
}

// ---------- actions (wired in main.js) ----------

export async function setIncomingEnabled(on) {
  try {
    applyState(await invoke("incoming_set_enabled", { enabled: on }));
  } catch {
    toast(t("error.unknown"));
    render();
  }
  refocus("#incEnabled");
}

export const incomingActions = {
  "inc-newcode": async () => {
    try {
      applyState(await invoke("incoming_new_code"));
    } catch {
      toast(t("error.unknown"));
    }
    refocus('[data-act="inc-newcode"]');
  },
  "inc-copy": async (el) => {
    try {
      await navigator.clipboard.writeText(el.dataset.text);
      toast(t(el.dataset.msg));
    } catch {
      toast(t("error.unknown"));
    }
  },
  "inc-view": () => answer("view"),
  "inc-control": () => answer("control"),
  "inc-reject": () => answer("reject"),
  "inc-mode": async (el) => {
    await invoke("incoming_set_mode", { mode: el.dataset.mode }).catch(() => toast(t("error.unknown")));
    refocus('[data-act="inc-mode"]');
  },
  "inc-disconnect": () => invoke("incoming_disconnect").catch(() => toast(t("error.unknown"))),
  "inc-demo-new": () => invoke("demo_incoming_request", { known: false }),
  "inc-demo-known": () => invoke("demo_incoming_request", { known: true }),
};

export async function startIncoming() {
  const { listen } = window.__TAURI__.event;
  await listen("incoming://request", (e) => openRequest(e.payload));
  await listen("incoming://session", (e) => {
    if (inc.pending) closeRequest();
    inc.session = e.payload;
    paintAll();
  });
  await listen("incoming://ended", (e) => {
    inc.session = null;
    paintAll();
    toast(t(`incoming.ended.${e.payload?.reason ?? "peer"}`));
  });
  setInterval(tick, TICK_MS);
  await refreshIncoming();
}
