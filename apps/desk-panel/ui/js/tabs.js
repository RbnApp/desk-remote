// Tab strip: one tab per connected PC, two-up button, Ctrl+Tab switching and file drop on a tab.

import { S, $, esc, go, ic, hooks as coreHooks, t } from "./core.js";
import { armHideAll, closeSession, focusedViewer, hooks, selectSession, sessions, syncActive, toggleSplit, workspace } from "./viewer.js";
import { openPick } from "./transfer.js";

const DOT_SHAPE = { online: "dot", connecting: "arc", lost: "tri" };

function tabHtml(viewer) {
  const id = esc(viewer.pc.id);
  const name = esc(viewer.pc.name);
  const state = viewer.tabState;
  const shown = S.screen === "viewer" && (workspace.split ? workspace.pair.includes(viewer.pc.id) : workspace.active === viewer.pc.id);
  const cls = `tab ${shown ? "on" : ""} ${workspace.split && workspace.pair.includes(viewer.pc.id) ? "pair" : ""}`;
  return `<div class="${cls}" data-pc="${id}" role="presentation">
    <button class="tab-main" role="tab" aria-selected="${shown}" data-act="tab-select" data-id="${id}" title="${name}">
      <span class="st ${state}">${ic(DOT_SHAPE[state], state === "connecting" ? "spin" : "")}</span><span class="tn">${name}</span><span class="sr"> — ${t(`tabs.state.${state}`)}</span></button>
    <button class="tab-x" data-act="tab-close" data-id="${id}" aria-label="${esc(t("tabs.close", { name: viewer.pc.name }))}" title="${esc(t("tabs.close", { name: viewer.pc.name }))}">${ic("x")}</button></div>`;
}

export function paintTabs() {
  const host = $("#tabs");
  const list = sessions();
  document.body.classList.toggle("tabs-on", list.length > 0);
  if (!list.length) {
    host.innerHTML = "";
    return;
  }
  const split = list.length > 1
    ? `<button class="btn sm ${workspace.split ? "primary" : ""}" data-act="tab-split" aria-pressed="${workspace.split}">${ic("rows")}${t("tabs.split")}</button>`
    : "";
  const held = document.activeElement?.closest("#tabs") ? document.activeElement.dataset : null;
  host.innerHTML = `<div class="tabbar" role="tablist" aria-label="${esc(t("tabs.label"))}">${list.map(tabHtml).join("")}
    <button class="btn sm icon" data-act="tab-add" aria-label="${esc(t("tabs.add"))}" title="${esc(t("tabs.add"))}">${ic("plus")}</button><span class="spacer"></span>${split}</div>`;
  // A rebuilt strip keeps the keyboard position.
  if (held?.act) host.querySelector(`[data-act="${held.act}"]${held.id ? `[data-id="${CSS.escape(held.id)}"]` : ""}`)?.focus({ preventScroll: true });
}

export const tabActions = {
  "tab-select": (el) => selectSession(el.dataset.id),
  "tab-close": (el) => closeSession(el.dataset.id),
  "tab-add": () => go("list"),
  "tab-split": () => toggleSplit(),
};

/** Ctrl+Tab / Ctrl+Shift+Tab walk the tabs; they stay local even while controlling a remote PC. */
function cycle(step) {
  const order = workspace.order;
  if (order.length < 2) return;
  const from = Math.max(0, order.indexOf(workspace.focus ?? workspace.active));
  selectSession(order[(from + step + order.length) % order.length]);
  focusedViewer()?.canvas.focus({ preventScroll: true });
}

document.addEventListener(
  "keydown",
  (e) => {
    if (e.key !== "Tab" || !e.ctrlKey || e.altKey || !$("#modal").hidden) return;
    e.preventDefault();
    e.stopPropagation();
    cycle(e.shiftKey ? -1 : 1);
  },
  true,
);

// ---------- dropping files on a tab ----------

const tabAt = (position) => {
  const ratio = window.devicePixelRatio || 1;
  return document.elementFromPoint(position.x / ratio, position.y / ratio)?.closest("#tabs .tab") ?? null;
};

const markDrop = (tab) => {
  document.querySelectorAll("#tabs .tab.drop").forEach((el) => el !== tab && el.classList.remove("drop"));
  tab?.classList.add("drop");
};

export async function startTabs() {
  const { listen } = window.__TAURI__.event;
  await listen("tauri://drag-over", (e) => markDrop(tabAt(e.payload.position)));
  await listen("tauri://drag-leave", () => markDrop(null));
  await listen("tauri://drag-drop", (e) => {
    const tab = tabAt(e.payload.position);
    markDrop(null);
    if (tab && e.payload.paths?.length) openPick(e.payload.paths, tab.dataset.pc);
  });
}

hooks.change = paintTabs;
coreHooks.afterRender = () => {
  syncActive();
  paintTabs();
  armHideAll();
};
