// PC list screen: cards or rows, status filter, search, empty/error/loading states.

import { S, $, closeModal, esc, ic, invoke, mountApp, openModal, render, screens, t, toast } from "./core.js";
import { PcCard, PcRow, Seg, Shell, StateBlock } from "./components.js";

const FILTERS = ["all", "online", "offline", "connecting", "warning"];

/** Asks, then removes the pairing at both ends (the PC is told only when reachable). */
export function confirmUnpair(id) {
  const pc = S.pcs.find((p) => p.id === id);
  if (!pc) return;
  openModal({
    key: "unpair",
    role: "alertdialog",
    icon: "alert",
    title: esc(t("list.unpair_title")),
    body: `<p>${esc(t("list.unpair_body", { name: pc.name }))}</p>`,
    focus: '[data-act="unpair-cancel"]',
    buttons: [
      { act: "unpair-cancel", label: t("list.unpair_cancel") },
      { act: "unpair-do", label: t("list.unpair"), cls: "primary", id: "unpairDo" },
    ],
  });
  S.unpairId = id;
}

export async function doUnpair() {
  const id = S.unpairId;
  closeModal("unpair");
  if (!id) return;
  try {
    await invoke("pc_unpair", { pcId: id });
    toast(t("list.unpair_done"));
  } catch {
    toast(t("error.unknown"));
  }
  S.unpairId = null;
  await loadPcs();
}

const RENAME_MAX = 40;

/** Asks for a new display name; it is kept in this panel only and blank returns to the real name. */
export function openRename(id) {
  const pc = S.pcs.find((p) => p.id === id);
  if (!pc) return;
  S.renameId = id;
  const real = pc.original_name || pc.name;
  const shown = pc.name === real ? "" : pc.name;
  openModal({
    key: "rename",
    icon: "edit",
    title: esc(t("list.rename_title")),
    body: `<label class="sr" for="renameName">${esc(t("list.rename_label"))}</label>
      <input id="renameName" class="input" type="text" maxlength="${RENAME_MAX}" autocomplete="off" value="${esc(shown)}" placeholder="${esc(real)}">
      <p class="hint">${esc(t("list.rename_hint", { name: real }))}</p>`,
    focus: "#renameName",
    buttons: [
      { act: "rename-cancel", label: t("list.rename_cancel") },
      { act: "rename-do", label: t("list.rename_save"), cls: "primary", id: "renameDo" },
    ],
  });
  $("#renameName")?.select();
}

export async function doRename() {
  const id = S.renameId;
  const name = ($("#renameName")?.value ?? "").trim();
  closeModal("rename");
  if (!id) return;
  try {
    await invoke("pc_rename", { pcId: id, name });
    toast(t(name ? "list.rename_done" : "list.rename_reset"));
  } catch {
    toast(t("error.unknown"));
  }
  S.renameId = null;
  await loadPcs();
}

export async function loadPcs() {
  S.listState = "loading";
  if (S.screen === "list") render();
  try {
    S.pcs = await invoke("list_pcs");
    S.listState = S.pcs.length ? "ready" : "empty";
  } catch {
    S.listState = "error";
  }
  if (S.screen === "list" || S.screen === "pair") render();
}

const REFRESH_MS = 5000;

/** Quiet refresh: statuses and latency update in place without the loading state. */
setInterval(async () => {
  if (S.screen !== "list" || S.listState === "loading" || S.listState === "error") return;
  const focused = document.activeElement;
  if (focused && focused.id === "q") return;
  try {
    const next = await invoke("list_pcs");
    if (JSON.stringify(next) === JSON.stringify(S.pcs) || S.screen !== "list") return;
    S.pcs = next;
    S.listState = next.length ? "ready" : "empty";
    refreshResults();
  } catch {
    /* keep the last known list */
  }
}, REFRESH_MS);

const count = (filter) => (filter === "all" ? S.pcs.length : S.pcs.filter((p) => p.status === filter).length);

function toolbar() {
  const chips = FILTERS.map(
    (f) =>
      `<button class="chip" data-filter="${f}" aria-pressed="${S.filter === f}">${f === "all" ? t("list.filter.all") : t(`status.${f}`)} <small>${count(f)}</small></button>`,
  ).join("");
  const views = Seg(
    "view",
    t("list.view.label"),
    [
      { v: "cards", l: t("list.view.cards"), icon: "grid" },
      { v: "rows", l: t("list.view.rows"), icon: "rows" },
    ],
    S.settings.list_view,
  );
  return `<div class="toolbar"><div class="search">${ic("search")}<label class="sr" for="q">${t("list.search")}</label><input id="q" class="input" type="search" placeholder="${esc(t("list.search_ph"))}" value="${esc(S.query)}" autocomplete="off"></div>
    <div class="chips" role="group" aria-label="${t("list.filter.label")}">${chips}</div><span class="spacer"></span>${views}</div>`;
}

function results() {
  const needle = S.query.trim().toLowerCase();
  const list = S.pcs.filter((p) => (S.filter === "all" || p.status === S.filter) && p.name.toLowerCase().includes(needle));
  if (!list.length) {
    return StateBlock("search", t("list.none.title"), t("list.none.body"), `<button class="btn" data-act="clear">${t("list.none.clear")}</button>`);
  }
  if (S.settings.list_view === "cards") return `<div class="grid">${list.map(PcCard).join("")}</div>`;
  const head = ["name", "status", "last"].map((c) => `<th scope="col">${t(`list.col.${c}`)}</th>`).join("");
  return `<table class="rows"><thead><tr>${head}<th scope="col"><span class="sr">${t("list.col.action")}</span></th></tr></thead><tbody>${list.map(PcRow).join("")}</tbody></table>`;
}

function body() {
  if (S.listState === "loading") {
    const card = `<div class="card" aria-hidden="true"><div class="thumb load"></div><div class="body"><div class="sk" style="width:60%"></div><div class="sk" style="width:40%"></div><div class="sk" style="height:36px;margin-top:8px"></div></div></div>`;
    return `<div role="status" aria-live="polite" class="sr">${t("list.loading")}</div><div class="grid">${card.repeat(6)}</div>`;
  }
  if (S.listState === "error") {
    return StateBlock(
      "wifi-off",
      t("list.err.title"),
      t("list.err.body"),
      `<button class="btn primary" data-act="retry">${ic("refresh")}${t("list.err.retry")}</button><button class="btn" data-go="settings">${t("list.err.settings")}</button>`,
    );
  }
  if (S.listState === "empty") {
    return StateBlock("monitor", t("list.empty.title"), t("list.empty.body"), `<button class="btn primary" data-go="pair">${ic("plus")}${t("list.empty.cta")}</button>`);
  }
  return `${toolbar()}<div id="results">${results()}</div>`;
}

function renderList() {
  const online = S.pcs.filter((p) => p.status === "online").length;
  const head = `<header class="head"><div class="grow"><h1>${t("list.title")}</h1><div class="sub">${t("list.sub", { n: S.pcs.length, on: online })}</div></div><button class="btn primary" data-go="pair">${ic("plus")}${t("list.add")}</button></header>`;
  const misplaced = S.info.agent_on_pc ? `<div class="msg warn" role="alert">${t("role.agent_on_panel_pc")}</div>` : "";
  mountApp(Shell(misplaced + body(), "list", head));
}

/** Search typing re-renders only the result block so the input keeps focus. */
export function refreshResults() {
  const host = $("#results");
  if (host) host.innerHTML = results();
}

screens.list = renderList;
