// File transfer UI: PC picker, progress tray, receive-side consent and the settings block.
// Commands/events follow the DAPP-85/87 contract: transfer_*, shell_menu_*, transfer://pick|progress|done
// (+ transfer://offer and transfer_inspect / transfer_pick_files / transfer_answer, see the report).

import { S, $, esc, ic, invoke, t, toast } from "./core.js";
import { dropWindowPin, surfaceWindow } from "./incoming.js";

const MIB = 1024 * 1024;
const OFFER_TIMEOUT_S = 30;
const TICK_MS = 1000;
const DONE_LINGER_MS = 12000;
const DEFAULT_LIMIT_MB = 100;
const MIN_LIMIT_MB = 1;
const MAX_LIMIT_MB = 1024;
const SPEED_SMOOTHING = 0.4;
const KNOWN_ERRORS = ["too_large", "denied", "no_answer", "busy", "disk_full", "unsupported_folder", "sha_mismatch", "canceled"];

/** Live transfer state; `available` is false when the build has no transfer commands. */
export const tx = {
  available: true,
  limitMb: DEFAULT_LIMIT_MB,
  limitBad: false,
  menu: null,
  menuAvailable: true,
  pick: null,
  offer: null,
  items: new Map(),
};

let offerLeft = 0;
let returnFocus = null;

// ---------- helpers ----------

export function humanSize(bytes) {
  if (bytes >= 1024 * MIB) return `${(bytes / (1024 * MIB)).toFixed(1)} GB`;
  if (bytes >= MIB) return `${(bytes / MIB).toFixed(bytes >= 10 * MIB ? 0 : 1)} MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${bytes} B`;
}

const baseName = (path) => String(path).split(/[\\/]/).pop();
const errText = (code) => t(`transfer.err.${KNOWN_ERRORS.includes(code) ? code : "unknown"}`);
const blockReason = (entry) => {
  if (entry.is_dir) return "folder";
  return entry.size > tx.limitMb * MIB ? "too_large" : "";
};

function lockPage(on) {
  $("#app").inert = on || Boolean($("#modal").innerHTML);
}

function remember() {
  returnFocus = document.activeElement;
}

function restoreFocus() {
  if (returnFocus?.isConnected) returnFocus.focus({ preventScroll: true });
  returnFocus = null;
}

// ---------- settings state ----------

export async function loadTransferState() {
  try {
    tx.limitMb = (await invoke("transfer_limits")).max_upload_mb;
    tx.available = true;
  } catch {
    tx.available = false;
  }
  try {
    const status = await invoke("shell_menu_status");
    tx.menu = typeof status === "boolean" ? status : Boolean(status?.enabled);
    tx.menuAvailable = true;
  } catch {
    tx.menuAvailable = false;
  }
}

export function transferSettings() {
  const demo = S.info.demo
    ? `<div class="inline"><button class="btn sm" data-act="tx-demo-pick">${t("transfer.demo.pick")}</button>
        <button class="btn sm" data-act="tx-demo-folder">${t("transfer.demo.folder")}</button>
        <button class="btn sm" data-act="tx-demo-offer">${t("transfer.demo.offer")}</button></div>`
    : "";
  let limit;
  if (tx.available) {
    limit = `<div class="field"><label for="txLimit">${t("transfer.set.limit")}</label>
      <input id="txLimit" class="input narrow-input" type="number" inputmode="numeric" min="${MIN_LIMIT_MB}" max="${MAX_LIMIT_MB}" step="1" value="${tx.limitMb}" aria-describedby="txLimitHint" ${tx.limitBad ? 'aria-invalid="true"' : ""}>
      <p class="hint ${tx.limitBad ? "bad" : ""}" id="txLimitHint" ${tx.limitBad ? 'role="alert"' : ""}>${t(tx.limitBad ? "transfer.set.limit_bad" : "transfer.set.limit_hint")}</p></div>`;
  } else {
    limit = `<div class="msg warn" role="status">${ic("alert")}${t("transfer.unavailable")}</div>`;
  }
  const menu = tx.menuAvailable
    ? `<div class="split"><div><h3 class="h3">${t("transfer.set.menu")}</h3><p class="hint">${t("transfer.set.menu_hint")}</p></div>
        <label class="sw"><input type="checkbox" id="shellMenu" role="switch" ${tx.menu ? "checked" : ""}><span class="tr"></span><span>${t(tx.menu ? "transfer.set.menu_on" : "transfer.set.menu_off")}</span></label></div>`
    : `<div class="msg warn" role="status">${ic("alert")}${t("transfer.set.menu_unavailable")}</div>`;
  return `<section class="sec" aria-labelledby="txTitle"><div><h2 id="txTitle">${t("transfer.set.title")}</h2><p class="hint">${t("transfer.set.hint")}</p></div>
    ${limit}<div class="divider"></div>${menu}${demo}</section>`;
}

async function saveLimit(input) {
  const value = Number(input.value);
  tx.limitBad = !Number.isInteger(value) || value < MIN_LIMIT_MB || value > MAX_LIMIT_MB;
  if (tx.limitBad) input.setAttribute("aria-invalid", "true");
  else input.removeAttribute("aria-invalid");
  const hint = $("#txLimitHint");
  hint.textContent = t(tx.limitBad ? "transfer.set.limit_bad" : "transfer.set.limit_hint");
  hint.classList.toggle("bad", tx.limitBad);
  if (tx.limitBad) return;
  try {
    await invoke("transfer_set_limit", { maxUploadMb: value });
    tx.limitMb = value;
    toast(t("transfer.set.saved"));
  } catch {
    toast(t("error.unknown"));
  }
}

async function saveMenu(input) {
  try {
    const result = await invoke("shell_menu_set", { enabled: input.checked });
    tx.menu = typeof result === "boolean" ? result : input.checked;
  } catch {
    input.checked = Boolean(tx.menu);
    toast(t("transfer.set.menu_failed"));
  }
  input.parentElement.lastElementChild.textContent = t(tx.menu ? "transfer.set.menu_on" : "transfer.set.menu_off");
}

// ---------- PC picker ----------

async function inspect(paths) {
  try {
    return await invoke("transfer_inspect", { paths });
  } catch {
    return paths.map((path) => ({ path, name: baseName(path), size: 0, is_dir: false }));
  }
}

async function freshPcs() {
  try {
    S.pcs = await invoke("list_pcs");
  } catch {
    // The last known list is still usable.
  }
  return S.pcs;
}

const reachable = (pc) => pc.status === "online" || pc.status === "warning";

function fileRow(entry) {
  const reason = blockReason(entry);
  const note = reason
    ? `<span class="badge warning">${ic("alert")}${reason === "folder" ? t("transfer.pick.folder") : t("transfer.pick.too_large", { mb: tx.limitMb })}</span>`
    : "";
  return `<li class="frow ${reason ? "blocked" : ""}"><span class="fname" title="${esc(entry.path)}">${esc(entry.name)}</span><span class="fsize">${entry.is_dir ? "" : humanSize(entry.size)}</span>${note}</li>`;
}

function pcOption(pc, selected) {
  const off = !reachable(pc);
  const badge = off
    ? `<span class="badge offline">${ic("ring")}${t("transfer.pick.offline")}</span>`
    : `<span class="badge online">${ic("dot")}${t("status.online")}</span>`;
  return `<label class="pcopt ${off ? "off" : ""}"><input type="radio" name="txpc" value="${esc(pc.id)}" ${selected === pc.id ? "checked" : ""} ${off ? "disabled" : ""}><span class="pn">${esc(pc.name)}</span>${badge}</label>`;
}

function pcListHtml() {
  const needle = tx.pick.query.trim().toLowerCase();
  const list = S.pcs.filter((p) => p.name.toLowerCase().includes(needle));
  if (!list.length) return `<p class="hint">${t("transfer.pick.none")}</p>`;
  return list.map((pc) => pcOption(pc, tx.pick.target)).join("");
}

function sendable() {
  return tx.pick.entries.filter((e) => !blockReason(e));
}

function pickFooterState() {
  const ok = sendable();
  const skipped = tx.pick.entries.length - ok.length;
  let hint = "";
  if (!ok.length) hint = t("transfer.pick.nothing");
  else if (!tx.pick.target) hint = t("transfer.pick.choose_pc");
  else if (skipped) hint = t("transfer.pick.skipped", { n: skipped });
  return { ready: Boolean(ok.length && tx.pick.target), hint };
}

function paintPickFooter() {
  const { ready, hint } = pickFooterState();
  const send = $("#txSend");
  send.toggleAttribute("disabled", !ready);
  send.setAttribute("aria-disabled", String(!ready));
  $("#txHint").textContent = hint;
}

function pickHtml() {
  const { entries } = tx.pick;
  return `<div class="modal-back"><div class="dialog wide" role="dialog" aria-modal="true" aria-labelledby="txTitleDlg">
    <div class="dlg-head"><div class="ico">${ic("send")}</div><div><h2 id="txTitleDlg">${t("transfer.pick.title")}</h2></div></div>
    <section aria-label="${esc(t("transfer.pick.files"))}"><ul class="flist">${entries.map(fileRow).join("")}</ul></section>
    <div class="search">${ic("search")}<label class="sr" for="txq">${t("transfer.pick.search")}</label><input id="txq" class="input" type="search" autocomplete="off" placeholder="${esc(t("transfer.pick.search_ph"))}"></div>
    <div class="pclist" id="txPcs" role="radiogroup" aria-label="${esc(t("transfer.pick.pcs"))}">${pcListHtml()}</div>
    <p class="hint" id="txHint" role="status"></p>
    <div class="dlg-act"><button class="btn" data-act="tx-pick-cancel" id="txCancel">${t("transfer.pick.cancel")}</button>
      <button class="btn primary" data-act="tx-pick-send" id="txSend" disabled aria-disabled="true">${ic("send")}${t("transfer.pick.send")}</button></div></div></div>`;
}

export async function openPick(paths, target = "") {
  if (!paths.length || tx.pick) return;
  await Promise.all([loadTransferState(), freshPcs()]);
  const entries = await inspect(paths);
  remember();
  tx.pick = { entries, query: "", target: reachable(S.pcs.find((p) => p.id === target) ?? {}) ? target : "" };
  $("#xmodal").innerHTML = pickHtml();
  lockPage(true);
  paintPickFooter();
  $("#txq").focus({ preventScroll: true });
}

function closePick() {
  tx.pick = null;
  $("#xmodal").innerHTML = "";
  lockPage(false);
  dropWindowPin();
  restoreFocus();
}

async function sendPick() {
  const { target } = tx.pick;
  const files = sendable();
  const pc = S.pcs.find((p) => p.id === target);
  closePick();
  try {
    const ids = await invoke("transfer_send", { deviceId: target, paths: files.map((f) => f.path) });
    ids.forEach((id, i) => addItem(id, { name: files[i]?.name ?? "", total: files[i]?.size ?? 0, peer: pc?.name ?? "", dir: "out" }));
  } catch (e) {
    toast(errText(String(e)));
  }
}

async function pickFilesFor(deviceId) {
  try {
    const paths = await invoke("transfer_pick_files");
    if (paths?.length) await openPick(paths, deviceId);
  } catch {
    toast(t("transfer.unavailable"));
  }
}

// ---------- transfer list ----------

function addItem(id, init) {
  tx.items.set(id, { id, sent: 0, total: 0, state: "run", speed: 0, at: Date.now(), error: "", ...init });
  paintTray();
}

const percent = (item) => (item.total ? Math.min(100, Math.floor((item.sent / item.total) * 100)) : 0);

function itemHtml(item) {
  const peer = item.peer ? ` <span class="peer">${t(item.dir === "in" ? "transfer.list.from" : "transfer.list.to", { pc: esc(item.peer) })}</span>` : "";
  const name = `<div class="tname"><span class="fname" title="${esc(item.name)}">${esc(item.name)}</span>${peer}</div>`;
  if (item.state === "run") {
    return `<li class="titem" data-tid="${esc(item.id)}">${name}
      <div class="tbar" role="progressbar" aria-label="${esc(item.name)}" aria-valuemin="0" aria-valuemax="100" aria-valuenow="${percent(item)}"><i style="width:${percent(item)}%"></i></div>
      <div class="tmeta"><span class="tpct">${percent(item)}%</span><span class="tspd">${item.speed ? t("transfer.list.speed", { v: humanSize(item.speed) }) : ""}</span><span class="spacer"></span>
      <button class="btn sm" data-act="tx-cancel" data-id="${esc(item.id)}" aria-label="${esc(t("transfer.list.cancel_named", { name: item.name }))}">${ic("x")}${t("transfer.list.cancel")}</button></div></li>`;
  }
  const ok = item.state === "ok";
  const text = ok
    ? item.dir === "in"
      ? t("transfer.list.received", { name: esc(item.name) })
      : t("transfer.list.sent", { name: esc(item.name), pc: esc(item.peer) })
    : t("transfer.list.failed", { name: esc(item.name), reason: errText(item.error) });
  return `<li class="titem done ${ok ? "ok" : "bad"}" data-tid="${esc(item.id)}" role="${ok ? "status" : "alert"}">${ic(ok ? "check" : "alert")}<span class="tdone">${text}</span>
    <button class="btn sm icon ghost" data-act="tx-dismiss" data-id="${esc(item.id)}" aria-label="${esc(t("transfer.list.dismiss"))}">${ic("x")}</button></li>`;
}

function paintTray() {
  const tray = $("#tray");
  const list = [...tx.items.values()];
  tray.innerHTML = list.length
    ? `<section class="tray" aria-label="${esc(t("transfer.list.title"))}"><h2 class="sr">${t("transfer.list.title")}</h2><ul>${list.map(itemHtml).join("")}</ul></section>`
    : "";
  paintBand();
}

/** Receive-side progress sits in the band at the top, like the session band. */
function paintBand() {
  const host = $("#xband");
  const active = [...tx.items.values()].filter((i) => i.dir === "in" && i.state === "run");
  host.innerHTML = active
    .map(
      (i) =>
        `<div class="band rx" role="status">${ic("copy")}<span class="bt"><b>${t("transfer.list.receiving", { name: esc(i.name) })}</b> · <span class="bm">${percent(i)}%</span></span><span class="spacer"></span><button class="btn sm danger" data-act="tx-cancel" data-id="${esc(i.id)}">${ic("x")}${t("transfer.list.cancel")}</button></div>`,
    )
    .join("");
}

function onProgress({ transfer_id: id, name, sent, total }) {
  let item = tx.items.get(id);
  if (!item) {
    addItem(id, { name, total, peer: "", dir: "out" });
    item = tx.items.get(id);
  }
  const now = Date.now();
  const rate = ((sent - item.sent) / Math.max(1, now - item.at)) * 1000;
  Object.assign(item, { sent, total, speed: item.speed ? item.speed + (rate - item.speed) * SPEED_SMOOTHING : rate, at: now });
  const row = document.querySelector(`.titem[data-tid="${CSS.escape(id)}"]`);
  const bar = row?.querySelector(".tbar");
  if (!bar) return paintTray();
  bar.setAttribute("aria-valuenow", percent(item));
  bar.firstElementChild.style.width = `${percent(item)}%`;
  row.querySelector(".tpct").textContent = `${percent(item)}%`;
  row.querySelector(".tspd").textContent = t("transfer.list.speed", { v: humanSize(item.speed) });
  const rx = document.querySelector(`#xband .band .bm`);
  if (rx && item.dir === "in") rx.textContent = `${percent(item)}%`;
}

function onDone({ transfer_id: id, name, ok, error }) {
  const item = tx.items.get(id) ?? (addItem(id, { name, peer: "", dir: "out" }), tx.items.get(id));
  Object.assign(item, { state: ok ? "ok" : "fail", error: error ?? "", sent: ok ? item.total : item.sent });
  paintTray();
  if (ok || error === "canceled") setTimeout(() => dismiss(id), DONE_LINGER_MS);
}

function dismiss(id) {
  if (tx.items.get(id)?.state === "run") return;
  tx.items.delete(id);
  paintTray();
}

// ---------- receive-side consent ----------

function offerHtml(offer) {
  return `<div class="modal-back top"><div class="dialog" role="alertdialog" aria-modal="true" aria-labelledby="ofTitle" aria-describedby="ofBody">
    <div class="dlg-head"><div class="ico">${ic("send")}</div><div><h2 id="ofTitle">${t("transfer.offer.title", { name: esc(offer.peer_name) })}</h2></div></div>
    <p class="hint" id="ofBody">${t("transfer.offer.body")}</p>
    <dl class="kv"><dt>${t("transfer.offer.file")}</dt><dd>${esc(offer.name)}</dd><dt>${t("transfer.offer.size")}</dt><dd>${humanSize(offer.size)}</dd></dl>
    <div class="cd" aria-hidden="true"><i id="ofBar"></i></div>
    <p class="hint" id="ofText">${t("transfer.offer.countdown", { s: offerLeft })}</p>
    <div class="dlg-act"><button class="btn danger" data-act="tx-reject" id="ofReject">${ic("x")}${t("transfer.offer.reject")}</button>
      <button class="btn primary" data-act="tx-accept">${ic("check")}${t("transfer.offer.accept")}</button></div></div></div>`;
}

function openOffer(offer) {
  if (tx.offer) {
    invoke("transfer_answer", { transferId: offer.transfer_id, accept: false, name: offer.name }).catch(() => {});
    return;
  }
  tx.offer = offer;
  offerLeft = OFFER_TIMEOUT_S;
  remember();
  $("#offer").innerHTML = offerHtml(offer);
  lockPage(true);
  surfaceWindow();
  // The safest answer owns the initial focus: an accidental Enter must not accept a file.
  $("#ofReject").focus({ preventScroll: true });
}

async function answerOffer(accept) {
  const offer = tx.offer;
  if (!offer) return;
  tx.offer = null;
  $("#offer").innerHTML = "";
  lockPage(Boolean(tx.pick));
  if (!tx.pick) dropWindowPin();
  restoreFocus();
  try {
    await invoke("transfer_answer", { transferId: offer.transfer_id, accept, name: offer.name });
    if (accept) addItem(offer.transfer_id, { name: offer.name, total: offer.size, peer: offer.peer_name, dir: "in" });
  } catch {
    toast(t("error.unknown"));
  }
}

function tick() {
  if (!tx.offer) return;
  offerLeft -= 1;
  if (offerLeft <= 0) return answerOffer(false);
  const text = $("#ofText");
  if (text) text.textContent = t("transfer.offer.countdown", { s: offerLeft });
  const bar = $("#ofBar");
  if (bar) bar.style.width = `${Math.round((offerLeft / OFFER_TIMEOUT_S) * 100)}%`;
}

// ---------- wiring ----------

export const transferActions = {
  "tx-card": (el) => pickFilesFor(el.dataset.id),
  "tx-pick-cancel": () => closePick(),
  "tx-pick-send": () => !$("#txSend").disabled && sendPick(),
  "tx-cancel": (el) => invoke("transfer_cancel", { transferId: el.dataset.id }).catch(() => toast(t("error.unknown"))),
  "tx-dismiss": (el) => dismiss(el.dataset.id),
  "tx-accept": () => answerOffer(true),
  "tx-reject": () => answerOffer(false),
  "tx-demo-pick": () => invoke("demo_transfer_pick", { folder: false }),
  "tx-demo-folder": () => invoke("demo_transfer_pick", { folder: true }),
  "tx-demo-offer": () => invoke("demo_transfer_offer"),
};

document.addEventListener("input", (e) => {
  if (e.target.id === "txq" && tx.pick) {
    tx.pick.query = e.target.value;
    $("#txPcs").innerHTML = pcListHtml();
    paintPickFooter();
  }
});

document.addEventListener("change", (e) => {
  const { id, name, value } = e.target;
  if (name === "txpc" && tx.pick) {
    tx.pick.target = value;
    paintPickFooter();
  } else if (id === "txLimit") {
    saveLimit(e.target);
  } else if (id === "shellMenu") {
    saveMenu(e.target);
  }
});

/** Escape cancels/rejects, Tab stays inside the topmost dialog. */
document.addEventListener("keydown", (e) => {
  const host = tx.offer ? "#offer" : tx.pick ? "#xmodal" : "";
  if (!host) return;
  if (e.key === "Escape") {
    e.preventDefault();
    if (tx.offer) answerOffer(false);
    else closePick();
  } else if (e.key === "Tab") {
    const items = [...document.querySelectorAll(`${host} button:not([disabled]), ${host} input:not([disabled])`)];
    const first = items[0];
    const last = items[items.length - 1];
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  } else if (e.key === "Enter" && tx.pick && e.target.id === "txq") {
    e.preventDefault();
    if (!$("#txSend").disabled) sendPick();
  }
});

export async function startTransfer() {
  const { listen } = window.__TAURI__.event;
  await listen("transfer://pick", async (e) => {
    await surfaceWindow();
    openPick(e.payload?.paths ?? []);
  });
  await listen("transfer://progress", (e) => onProgress(e.payload));
  await listen("transfer://done", (e) => onDone(e.payload));
  await listen("transfer://offer", (e) => openOffer(e.payload));
  setInterval(tick, TICK_MS);
  await loadTransferState();
}
