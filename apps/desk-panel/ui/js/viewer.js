// Live viewers: one Viewer per connected PC (tab) with its own pane, toolbar, overlays and
// reconnection. The tab strip and the drag-and-drop live in tabs.js and drive this module through
// `workspace`, `layout()`, `openViewer()` and `hooks`.

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

/** What the window shows: open tabs in order, the active one, and the optional two-up pair. */
export const workspace = { order: [], active: null, split: false, pair: [], focus: null, fs: false };

/** tabs.js hooks in here to repaint the strip when a session changes state. */
export const hooks = { change() {} };

const viewers = new Map();
let nextUid = 1;

export const viewerOf = (pcId) => viewers.get(pcId);
export const focusedViewer = () => viewers.get(workspace.focus) ?? null;
export const paneViewer = (el) => viewers.get(el.closest(".vpane")?.dataset.pc);
export const sessions = () => workspace.order.map((id) => viewers.get(id));
export const visibleIds = () => (S.screen !== "viewer" ? [] : workspace.split ? workspace.pair : [workspace.active]);

const latencyClass = (ms) => (ms < LATENCY_GOOD_MS ? "good" : ms < LATENCY_FAIR_MS ? "mid" : "bad");

function overlayCard(kind, icon, title, body, buttons) {
  const role = kind === "load" ? "status" : "alert";
  return `<div class="overlay" role="${role}"><div class="ov-card"><div class="ico ${kind}">${icon}</div><h2>${title}</h2><p class="hint">${body}</p>${buttons ? `<div class="row">${buttons}</div>` : ""}</div></div>`;
}

/** One live connection to one PC: its own pane, canvas, decoder, channel and input binding. */
class Viewer {
  constructor(pc) {
    this.pc = pc;
    this.uid = nextUid++;
    this.phase = "connecting"; // connecting | live | lost | failed | ended | unsupported
    this.control = false;
    this.hostViewOnly = false;
    this.active = true;
    this.fps = 0;
    this.rtt = 0;
    this.decodeMs = 0;
    this.hardware = null;
    this.retry = 1;
    this.reason = "";
    this.token = 0;
    this.pipeline = null;
    this.retryTimer = 0;
    this.hideTimer = 0;
    this.chain = Promise.resolve();
    this.el = this.buildPane();
    this.canvas = this.el.querySelector("canvas");
    this.releaseKeys = attachInput(
      this.canvas,
      (event) => invoke("viewer_input", { pcId: this.pc.id, event }).catch(() => {}),
      () => this.control && !this.hostViewOnly && this.phase === "live" && workspace.focus === this.pc.id,
    );
    this.paint();
  }

  buildPane() {
    const el = document.createElement("section");
    el.className = "vpane";
    el.dataset.pc = this.pc.id;
    el.setAttribute("aria-label", this.pc.name);
    el.innerHTML = `<div class="vbar" role="toolbar" aria-label="${esc(this.pc.name)}"></div>
      <div class="stage"><div class="screen viewonly"><canvas tabindex="0" width="1280" height="720" aria-label="${esc(this.pc.name)}"></canvas></div><div class="overlay-host"></div></div>`;
    // Capture phase: the pane becomes the focused side before the canvas handles the click.
    el.addEventListener("pointerdown", () => setFocus(this.pc.id), true);
    return el;
  }

  get latencyMs() {
    return Math.round(this.rtt + this.decodeMs);
  }

  /** Dot state shown on the tab. */
  get tabState() {
    if (this.phase === "live") return "online";
    return this.phase === "connecting" ? "connecting" : "lost";
  }

  toolbar() {
    const live = this.phase === "live";
    const badge = Badge(live ? "online" : this.phase === "connecting" ? "connecting" : "offline");
    const ms = this.latencyMs;
    const cls = latencyClass(ms);
    const note = `hostNote-${this.uid}`;
    const fsLabel = workspace.fs ? t("view.exit_fullscreen") : t("view.fullscreen");
    const liveParts = live
      ? `<span class="stat ${cls}" data-stat="lat"><span>${t("view.latency")}</span><b>${ms}</b>ms<span class="sr">${t(`view.lat.${cls}`)}</span></span>
       <span class="stat" data-stat="fps"><b>${this.fps}</b>fps</span>
       <span class="dec">${this.hardware ? t("view.decoder.hw") : t("view.decoder.sw")}</span><span class="sep"></span>
       ${this.hostViewOnly ? `<span class="pill host" id="${note}">${ic("alert")}<span>${t("view.hostviewonly.pill")}</span></span>` : ""}<label class="sw ${this.hostViewOnly ? "locked" : ""}" title="${esc(this.hostViewOnly ? t("view.hostviewonly.tip", { name: this.pc.name }) : this.control ? t("view.mode.control_hint") : t("view.mode.view_hint"))}"><input type="checkbox" data-ctl ${this.control ? "checked" : ""} ${this.hostViewOnly ? `aria-disabled="true" aria-describedby="${note}"` : ""} role="switch"><span class="tr"></span><span class="pill ${this.control ? "ctl" : ""}">${ic(this.control ? "pointer" : "eye")}<span>${this.control ? t("view.mode.control") : t("view.mode.view")}</span></span><span class="sr">${t("view.control.toggle")}</span></label>`
      : "";
    const demo = S.info.demo ? `<button class="btn sm" data-act="demo-drop">${ic("wifi-off")}<span class="opt">${t("view.demo.drop")}</span></button>` : "";
    return `<button class="btn sm" data-act="disconnect">${ic("power")}<span class="opt">${t("view.disconnect")}</span><span class="sr">${t("view.disconnect")}</span></button>
    <span class="sep"></span><span class="vname" title="${esc(this.pc.name)}">${esc(this.pc.name)}</span>${badge}<span class="spacer"></span>
    ${liveParts}${demo}
    <button class="btn sm icon" data-act="fs" aria-label="${fsLabel}" title="${fsLabel}">${ic(workspace.fs ? "shrink" : "expand")}</button>`;
  }

  overlay() {
    const name = esc(this.pc.name);
    const back = `<button class="btn" data-act="disconnect">${t("tabs.close_session")}</button>`;
    const retry = `<button class="btn primary" data-act="v-retry">${ic("refresh")}${t("view.lost.retry")}</button>`;
    switch (this.phase) {
      case "connecting":
        return overlayCard("load", ic("arc", "spin"), t("view.connecting.title", { name }), t("view.connecting.body"), "");
      case "lost":
        return overlayCard("bad", ic("wifi-off"), t("view.lost.title"), t("view.lost.body", { name, n: this.retry, max: MAX_RETRY }), retry + back);
      case "ended":
        return overlayCard("bad", ic("power"), t("view.ended.title", { name }), t("view.ended.body"), back);
      case "failed":
        if (this.reason === "denied") return overlayCard("bad", ic("x"), t("view.denied.title", { name }), t("view.denied.body"), retry + back);
        if (this.reason === "no_answer") return overlayCard("bad", ic("clock"), t("view.noanswer.title", { name }), t("view.noanswer.body"), retry + back);
        if (this.reason === "unpaired") return overlayCard("bad", ic("alert"), t("view.lost.unpaired_title"), t("view.lost.unpaired_body", { name }), back);
        return overlayCard("bad", ic("alert"), t("view.lost.fail_title"), t("view.lost.fail_body", { name, max: MAX_RETRY }), retry + back);
      case "unsupported":
        return overlayCard("bad", ic("alert"), t("view.unsupported.title"), t("view.unsupported.body"), back);
      default:
        return "";
    }
  }

  paint() {
    this.el.querySelector(".vbar").innerHTML = this.toolbar();
    this.el.querySelector(".overlay-host").innerHTML = this.overlay();
    this.el.querySelector(".screen").className = `screen ${this.control ? "control" : "viewonly"}`;
    this.armHide();
    hooks.change();
  }

  paintStats() {
    if (this.phase !== "live") return;
    const ms = this.latencyMs;
    const cls = latencyClass(ms);
    const lat = this.el.querySelector('[data-stat="lat"]');
    if (lat) {
      lat.className = `stat ${cls}`;
      lat.innerHTML = `<span>${t("view.latency")}</span><b>${ms}</b>ms<span class="sr">${t(`view.lat.${cls}`)}</span>`;
    }
    const fps = this.el.querySelector('[data-stat="fps"]');
    if (fps) fps.innerHTML = `<b>${this.fps}</b>fps`;
  }

  /** Shows the toolbar again on activity and hides it after a pause in fullscreen. */
  armHide() {
    const bar = this.el.querySelector(".vbar");
    bar.classList.remove("hide");
    clearTimeout(this.hideTimer);
    if (workspace.fs && this.phase === "live") {
      this.hideTimer = setTimeout(() => {
        if (!bar.contains(document.activeElement)) bar.classList.add("hide");
      }, HIDE_DELAY_MS);
    }
  }

  requestKeyframe() {
    invoke("viewer_request_keyframe", { pcId: this.pc.id }).catch(() => {});
  }

  /** A background tab stops its video stream; coming back asks for a fresh picture. */
  setActive(on) {
    if (on === this.active) return;
    this.active = on;
    invoke("viewer_set_active", { pcId: this.pc.id, active: on }).catch(() => {});
    if (on) this.requestKeyframe();
    else this.releaseKeys();
  }

  newPipeline() {
    this.pipeline?.close();
    this.pipeline = new VideoPipeline(this.canvas, {
      onFirstFrame: () => {
        this.phase = "live";
        this.retry = 1;
        this.paint();
      },
      onNeedKeyframe: () => this.requestKeyframe(),
      onStats: ({ fps, decodeMs }) => {
        this.fps = fps;
        this.decodeMs = decodeMs;
        this.paintStats();
      },
      onUnsupported: () => {
        this.phase = "unsupported";
        this.paint();
      },
    });
  }

  async handleMessage(message) {
    if (message instanceof ArrayBuffer || ArrayBuffer.isView(message)) {
      this.pipeline?.push(parseFrame(message));
      return;
    }
    switch (message.type) {
      case "config":
        await this.pipeline?.configure(message.codec);
        this.hardware = this.pipeline?.hardware ?? null;
        break;
      case "rtt":
        this.rtt = message.ms;
        break;
      case "notice":
        this.onHostNotice(message.kind);
        break;
      case "closed":
        if (this.phase === "live" || this.phase === "connecting") this.enterLost();
        break;
      default:
    }
  }

  onHostNotice(kind) {
    const name = this.pc.name;
    if (kind === "view_only_on") {
      this.hostViewOnly = true;
      this.control = false;
      this.paint();
      toast(t("view.toast.host_viewonly_on", { name }));
    } else if (kind === "view_only_off") {
      this.hostViewOnly = false;
      this.paint();
      toast(t("view.toast.host_viewonly_off"));
    } else if (kind === "disconnected_by_host") {
      this.token += 1;
      clearTimeout(this.retryTimer);
      this.pipeline?.close();
      this.pipeline = null;
      this.phase = "ended";
      this.paint();
      this.el.querySelector(".overlay [data-act]")?.focus({ preventScroll: true });
    }
  }

  async startSession() {
    const token = ++this.token;
    const channel = newChannel();
    channel.onmessage = (message) => {
      if (token !== this.token) return;
      this.chain = this.chain.then(() => this.handleMessage(message)).catch(() => {});
    };
    this.newPipeline();
    this.phase = "connecting";
    this.paint();
    await invoke("viewer_connect", { pcId: this.pc.id, events: channel });
    // The tab may have been sent to the background while the link was still being set up.
    if (!this.active) invoke("viewer_set_active", { pcId: this.pc.id, active: false }).catch(() => {});
  }

  async attempt() {
    this.reason = "";
    try {
      await this.startSession();
    } catch (reason) {
      this.reason = typeof reason === "string" ? reason : "";
      if (this.reason === "unpaired" || this.reason === "denied" || this.reason === "no_answer") {
        this.token += 1;
        this.phase = "failed";
        this.paint();
        return;
      }
      this.enterLost(this.retry);
    }
  }

  enterLost(retry = 1) {
    this.token += 1;
    this.pipeline?.close();
    this.pipeline = null;
    this.phase = "lost";
    this.retry = retry;
    this.paint();
    clearTimeout(this.retryTimer);
    this.retryTimer = setTimeout(() => {
      if (this.phase !== "lost") return;
      if (this.retry >= MAX_RETRY) {
        this.phase = "failed";
        this.paint();
        return;
      }
      this.retry += 1;
      this.paint();
      this.attempt();
    }, RETRY_DELAY_MS);
  }

  retryNow() {
    clearTimeout(this.retryTimer);
    this.retry = 1;
    this.attempt();
  }

  setControl(on) {
    const switchEl = () => this.el.querySelector("[data-ctl]");
    if (this.hostViewOnly) {
      this.paint();
      toast(t("view.hostviewonly.tip", { name: this.pc.name }));
      switchEl()?.focus({ preventScroll: true });
      return;
    }
    this.control = on;
    this.paint();
    toast(on ? t("view.toast.control") : t("view.toast.viewonly"));
    switchEl()?.focus({ preventScroll: true });
  }

  /** Ends this connection and frees its decoder and timers. */
  async dispose() {
    this.token += 1;
    clearTimeout(this.retryTimer);
    clearTimeout(this.hideTimer);
    this.releaseKeys();
    this.pipeline?.close();
    this.pipeline = null;
    this.el.remove();
    await invoke("viewer_disconnect", { pcId: this.pc.id }).catch(() => {});
  }
}

/** Builds the viewer for a PC (one pane, canvas, decoder, channel and input binding each). */
export function createViewer(pc) {
  return new Viewer(pc);
}

// ---------- workspace: which viewers are shown ----------

const markFocus = () => {
  for (const v of viewers.values()) v.el.classList.toggle("focused", workspace.split && v.pc.id === workspace.focus);
};

export function setFocus(pcId) {
  if (workspace.focus === pcId) return;
  workspace.focus = pcId;
  markFocus();
  hooks.change();
}

/** Tells the backend which sessions are on screen; the rest stop streaming. */
export function syncActive() {
  const visible = visibleIds();
  for (const v of viewers.values()) v.setActive(visible.includes(v.pc.id));
}

/** Puts the visible pane(s) into the workspace and refreshes tabs and activity. */
export function layout() {
  const host = $("#workspace");
  if (host && workspace.active) {
    const shown = (workspace.split ? workspace.pair : [workspace.active]).map((id) => viewers.get(id).el);
    host.className = `workspace${workspace.split ? " two-up" : ""}`;
    host.replaceChildren(...shown);
    markFocus();
  }
  syncActive();
  hooks.change();
}

function renderViewer() {
  if (!workspace.active) {
    go("list");
    return;
  }
  mountApp(`<main class="workspace" id="workspace"></main>`, `app immersive${workspace.fs ? " fs" : ""}`);
  // Panes outlive screen changes, so their texts are refreshed here (language may have changed).
  for (const v of viewers.values()) v.paint();
  layout();
}

/** Makes a tab the visible one (in two-up mode it takes the focused side). */
export function selectSession(pcId) {
  if (workspace.split && !workspace.pair.includes(pcId)) {
    workspace.pair[Math.max(0, workspace.pair.indexOf(workspace.focus))] = pcId;
  }
  workspace.active = pcId;
  workspace.focus = pcId;
  if (S.screen === "viewer") layout();
  else go("viewer");
}

/** Opens a PC as a new tab, or switches to its tab when it is already open. */
export function openViewer(pcId) {
  if (viewers.has(pcId)) {
    selectSession(pcId);
    return;
  }
  const pc = S.pcs.find((p) => p.id === pcId);
  if (!pc || pc.status === "offline" || pc.status === "connecting") return;
  const viewer = createViewer(pc);
  viewers.set(pcId, viewer);
  workspace.order.push(pcId);
  selectSession(pcId);
  viewer.attempt();
}

/** Closes one session; the others stay connected. The last one returns to the PC list. */
export async function closeSession(pcId) {
  const viewer = viewers.get(pcId);
  if (!viewer) return;
  const at = workspace.order.indexOf(pcId);
  viewers.delete(pcId);
  workspace.order.splice(at, 1);
  const next = workspace.order[Math.min(at, workspace.order.length - 1)] ?? null;
  const side = workspace.pair.indexOf(pcId);
  if (workspace.split && side >= 0) {
    const spare = workspace.order.find((id) => !workspace.pair.includes(id));
    if (spare) workspace.pair[side] = spare;
    else workspace.split = false;
  }
  if (workspace.active === pcId) workspace.active = workspace.split ? workspace.pair[0] : next;
  if (workspace.focus === pcId) workspace.focus = workspace.active;
  if (workspace.split && !workspace.pair.includes(workspace.focus)) workspace.focus = workspace.pair[0];
  await viewer.dispose();
  if (!workspace.order.length) {
    if (workspace.fs) await toggleFullscreen();
    Object.assign(workspace, { active: null, focus: null, split: false, pair: [] });
    go("list");
    loadPcs();
    return;
  }
  if (S.screen === "viewer") layout();
  else hooks.change();
}

/** Two-up mode: the active tab next to the next one, or back to a single view. */
export function toggleSplit() {
  if (workspace.split) {
    workspace.split = false;
    workspace.active = workspace.focus ?? workspace.active;
  } else if (workspace.order.length > 1) {
    const other = workspace.order[(workspace.order.indexOf(workspace.active) + 1) % workspace.order.length];
    workspace.split = true;
    workspace.pair = [workspace.active, other];
    workspace.focus = workspace.active;
  }
  if (S.screen === "viewer") layout();
  else hooks.change();
}

export async function toggleFullscreen() {
  workspace.fs = !workspace.fs;
  await appWindow().setFullscreen(workspace.fs).catch(() => {});
  document.body.classList.toggle("fs", workspace.fs);
  $("#app").className = `app immersive${workspace.fs ? " fs" : ""}`;
  for (const v of viewers.values()) v.paint();
}

export function armHideAll() {
  for (const v of viewers.values()) v.armHide();
}

/** Buttons inside a pane act on that pane's viewer. */
export const viewerActions = {
  disconnect: (el) => closeSession(paneViewer(el)?.pc.id),
  fs: () => toggleFullscreen(),
  "v-retry": (el) => paneViewer(el)?.retryNow(),
  "demo-drop": (el) => invoke("demo_viewer_drop", { pcId: paneViewer(el)?.pc.id }).catch(() => {}),
};

export const setControlFrom = (el) => paneViewer(el)?.setControl(el.checked);

screens.viewer = renderViewer;
