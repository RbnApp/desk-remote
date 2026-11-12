// PC list screen: cards or rows, status filter, search, empty/error/loading states.

import { S, $, esc, ic, invoke, mountApp, render, screens, t } from "./core.js";
import { PcCard, PcRow, Seg, Shell, StateBlock } from "./components.js";

const FILTERS = ["all", "online", "offline", "connecting", "warning"];

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
  mountApp(Shell(body(), "list", head));
}

/** Search typing re-renders only the result block so the input keeps focus. */
export function refreshResults() {
  const host = $("#results");
  if (host) host.innerHTML = results();
}

screens.list = renderList;
