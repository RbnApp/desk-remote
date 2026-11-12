// Live viewer: session lifecycle, toolbar, overlays, reconnection, fullscreen.

import { S, $, appWindow, esc, go, ic, invoke, mountApp, newChannel, screens, t, toast } from "./core.js";
import { Badge } from "./components.js";
import { VideoPipeline, parseFrame } from "./decoder.js";
import { attachInput } from "./input.js";
import { loadPcs } from "./list.js";

const MAX_RETRY = 5;
const RETRY_DELAY_MS = 2200;
const HIDE_DELAY_MS = 3000;
const LATENCY_GOOD_MS = 80;
const LATENCY_FAIR_MS = 200;

const V = {
  pc: null,
  phase: "connecting", // connecting | live | lost | failed | unsupported
  control: false,
  fs: false,
  fps: 0,
  rtt: 0,
  decodeMs: 0,
  hardware: null,
  retry: 1,
  reason: "",
  token: 0,
  pipeline: null,
  retryTimer: null,
  hideTimer: null,
  chain: Promise.resolve(),
};

const latencyMs = () => Math.round(V.rtt + V.decodeMs);
const latencyClass = (ms) => (ms < LATENCY_GOOD_MS ? "good" : ms < LATENCY_FAIR_MS ? "mid" : "bad");

function toolbar() {
  const live = V.phase === "live";
  const badge = Badge(live ? "online" : V.phase === "connecting" ? "connecting" : "offline");
  const ms = latencyMs();
  const cls = latencyClass(ms);
  const fsLabel = V.fs ? t("view.exit_fullscreen") : t("view.fullscreen");
  const liveParts = live
    ? `<span class="stat ${cls}" id="stLat"><span>${t("view.latency")}</span><b>${ms}</b>ms<span class="sr">${t(`view.lat.${cls}`)}</span></span>
       <span class="stat" id="stFps"><b>${V.fps}</b>fps</span>
       <span class="dec">${V.hardware ? t("view.decoder.hw") : t("view.decoder.sw")}</span><span class="sep"></span>
       <label class="sw" title="${esc(V.control ? t("view.mode.control_hint") : t("view.mode.view_hint"))}"><input type="checkbox" id="ctl" ${V.control ? "checked" : ""} role="switch"><span class="tr"></span><span class="pill ${V.control ? "ctl" : ""}">${ic(V.control ? "pointer" : "eye")}<span>${V.control ? t("view.mode.control") : t("view.mode.view")}</span></span><span class="sr">${t("view.control.toggle")}</span></label>`
    : "";
  const demo = S.info.demo ? `<button class="btn sm" data-act="demo-drop">${ic("wifi-off")}<span class="opt">${t("view.demo.drop")}</span></button>` : "";
  return `<button class="btn sm" data-act="disconnect">${ic("power")}<span class="opt">${t("view.disconnect")}</span><span class="sr">${t("view.disconnect")}</span></button>
    <span class="sep"></span><span class="vname" title="${esc(V.pc.name)}">${esc(V.pc.name)}</span>${badge}<span class="spacer"></span>
    ${liveParts}${demo}
    <button class="btn sm icon" data-act="fs" aria-label="${fsLabel}" title="${fsLabel}">${ic(V.fs ? "shrink" : "expand")}</button>`;
}

function overlayCard(kind, icon, title, body, buttons) {
  const role = kind === "load" ? "status" : "alert";
  return `<div class="overlay" role="${role}"><div class="ov-card"><div class="ico ${kind}">${icon}</div><h2>${title}</h2><p class="hint">${body}</p>${buttons ? `<div class="row">${buttons}</div>` : ""}</div></div>`;
}

function overlay() {
  const name = esc(V.pc.name);
  const back = `<button class="btn" data-act="disconnect">${t("view.back")}</button>`;
  const retry = `<button class="btn primary" data-act="v-retry">${ic("refresh")}${t("view.lost.retry")}</button>`;
  switch (V.phase) {
    case "connecting":
      return overlayCard("load", ic("arc", "spin"), t("view.connecting.title", { name }), t("view.connecting.body"), "");
    case "lost":
      return overlayCard("bad", ic("wifi-off"), t("view.lost.title"), t("view.lost.body", { name, n: V.retry, max: MAX_RETRY }), retry + back);
    case "failed":
      if (V.reason === "unpaired") {
        return overlayCard("bad", ic("alert"), t("view.lost.unpaired_title"), t("view.lost.unpaired_body", { name }), back);
      }
      return overlayCard("bad", ic("alert"), t("view.lost.fail_title"), t("view.lost.fail_body", { name, max: MAX_RETRY }), retry + back);
    case "unsupported":
      return overlayCard("bad", ic("alert"), t("view.unsupported.title"), t("view.unsupported.body"), back);
    default:
      return "";
  }
}

function paint() {
  const bar = $("#vbar");
  if (bar) bar.innerHTML = toolbar();
  const host = $("#overlay");
  if (host) host.innerHTML = overlay();
  const screen = $("#screen");
  if (screen) screen.className = `screen ${V.control ? "control" : "viewonly"}`;
  armHide();
}

function renderViewer() {
  if (!V.pc) {
    go("list");
    return;
  }
  mountApp(
    `<main class="viewer"><div class="vbar" id="vbar" role="toolbar" aria-label="${esc(V.pc.name)}"></div>
     <div class="stage" id="stage"><div class="screen viewonly" id="screen"><canvas id="remote" tabindex="0" width="1280" height="720" aria-label="${esc(V.pc.name)}"></canvas></div><div id="overlay"></div></div></main>`,
    `app immersive${V.fs ? " fs" : ""}`,
  );
  attachInput($("#remote"), sendInput, () => V.control && V.phase === "live");
  paint();
}

function sendInput(event) {
  invoke("viewer_input", { event }).catch(() => {});
}

const requestKeyframe = () => invoke("viewer_request_keyframe").catch(() => {});

function newPipeline() {
  V.pipeline?.close();
  V.pipeline = new VideoPipeline($("#remote"), {
    onFirstFrame: () => {
      V.phase = "live";
      V.retry = 1;
      paint();
    },
    onNeedKeyframe: requestKeyframe,
    onStats: ({ fps, decodeMs }) => {
      V.fps = fps;
      V.decodeMs = decodeMs;
      paintStats();
    },
    onUnsupported: () => {
      V.phase = "unsupported";
      paint();
    },
  });
}

function paintStats() {
  if (V.phase !== "live") return;
  const ms = latencyMs();
  const cls = latencyClass(ms);
  const lat = $("#stLat");
  if (lat) {
    lat.className = `stat ${cls}`;
    lat.innerHTML = `<span>${t("view.latency")}</span><b>${ms}</b>ms<span class="sr">${t(`view.lat.${cls}`)}</span>`;
  }
  const fps = $("#stFps");
  if (fps) fps.innerHTML = `<b>${V.fps}</b>fps`;
}

async function handleMessage(message) {
  if (message instanceof ArrayBuffer || ArrayBuffer.isView(message)) {
    V.pipeline?.push(parseFrame(message));
    return;
  }
  switch (message.type) {
    case "config":
      await V.pipeline?.configure(message.codec);
      V.hardware = V.pipeline?.hardware ?? null;
      break;
    case "rtt":
      V.rtt = message.ms;
      break;
    case "closed":
      if (V.phase === "live" || V.phase === "connecting") enterLost();
      break;
    default:
  }
}

async function startSession() {
  const token = ++V.token;
  const channel = newChannel();
  channel.onmessage = (message) => {
    if (token !== V.token) return;
    V.chain = V.chain.then(() => handleMessage(message)).catch(() => {});
  };
  newPipeline();
  V.phase = "connecting";
  paint();
  await invoke("viewer_connect", { pcId: V.pc.id, events: channel });
}

async function attempt() {
  V.reason = "";
  try {
    await startSession();
  } catch (reason) {
    V.reason = typeof reason === "string" ? reason : "";
    if (V.reason === "unpaired") {
      V.token += 1;
      V.phase = "failed";
      paint();
      return;
    }
    enterLost(V.retry);
  }
}

function enterLost(retry = 1) {
  V.token += 1;
  V.pipeline?.close();
  V.pipeline = null;
  V.phase = "lost";
  V.retry = retry;
  paint();
  clearTimeout(V.retryTimer);
  V.retryTimer = setTimeout(() => {
    if (V.phase !== "lost") return;
    if (V.retry >= MAX_RETRY) {
      V.phase = "failed";
      paint();
      return;
    }
    V.retry += 1;
    paint();
    attempt();
  }, RETRY_DELAY_MS);
}

export function openViewer(pcId) {
  const pc = S.pcs.find((p) => p.id === pcId);
  if (!pc || pc.status === "offline" || pc.status === "connecting") return;
  Object.assign(V, { pc, control: false, fps: 0, rtt: 0, decodeMs: 0, hardware: null, retry: 1 });
  S.screen = "viewer";
  renderViewer();
  attempt();
}

export async function closeViewer() {
  V.token += 1;
  clearTimeout(V.retryTimer);
  clearTimeout(V.hideTimer);
  V.pipeline?.close();
  V.pipeline = null;
  await invoke("viewer_disconnect").catch(() => {});
  if (V.fs) await appWindow().setFullscreen(false).catch(() => {});
  V.fs = false;
  V.pc = null;
  S.screen = "list";
  go("list");
  loadPcs();
}

export function retryNow() {
  clearTimeout(V.retryTimer);
  V.retry = 1;
  attempt();
}

export function setControl(on) {
  V.control = on;
  paint();
  toast(on ? t("view.toast.control") : t("view.toast.viewonly"));
  $("#ctl")?.focus({ preventScroll: true });
}

export async function toggleFullscreen() {
  V.fs = !V.fs;
  await appWindow().setFullscreen(V.fs).catch(() => {});
  $("#app").className = `app immersive${V.fs ? " fs" : ""}`;
  paint();
}

export const dropDemoConnection = () => invoke("demo_drop").catch(() => {});

/** Shows the toolbar again on activity and hides it after a pause in fullscreen. */
export function armHide() {
  const bar = $("#vbar");
  if (!bar) return;
  bar.classList.remove("hide");
  clearTimeout(V.hideTimer);
  if (V.fs && V.phase === "live") {
    V.hideTimer = setTimeout(() => {
      const el = $("#vbar");
      if (el && !el.contains(document.activeElement)) el.classList.add("hide");
    }, HIDE_DELAY_MS);
  }
}

export const viewerState = V;

screens.viewer = renderViewer;
