// Updates tab: installed version, status, "check now" / "update" and the start-up switch.
// Rust (update.rs) does the work; this file only renders the `update://status` value.

import { S, esc, ic, invoke, render, t } from "./core.js";

let up = { current: "", state: "idle", latest: null, received: 0, total: null, last_check: null, error: null };

const BADGE = {
  idle: "online",
  checking: "connecting",
  available: "warning",
  downloading: "connecting",
  ready: "online",
  waiting: "warning",
  installing: "connecting",
  failed: "offline",
};
const BUSY = ["checking", "downloading", "waiting", "installing"];

function lastCheck() {
  if (!up.last_check) return t("upd.never");
  const locale = S.lang === "tr" ? "tr-TR" : "en-GB";
  return new Date(up.last_check * 1000).toLocaleString(locale, { dateStyle: "medium", timeStyle: "short" });
}

function progress() {
  if (up.state !== "downloading") return "";
  const percent = up.total ? Math.min(100, Math.round((up.received / up.total) * 100)) : 0;
  return `<div class="tbar" role="progressbar" aria-label="${esc(t("upd.progress"))}" aria-valuemin="0" aria-valuemax="100" aria-valuenow="${percent}"><i style="width:${percent}%"></i></div>`;
}

export function updateSettings() {
  const busy = BUSY.includes(up.state);
  const offered = ["available", "failed"].includes(up.state) && up.latest;
  const apply = offered
    ? `<button class="btn primary" data-act="upd-apply">${ic("check")}${t("upd.apply")}</button>`
    : up.state === "waiting"
      ? `<button class="btn primary" disabled>${ic("arc", "spin")}${t("upd.apply_waiting")}</button>`
      : "";
  const failed = up.state === "failed" ? `<div class="msg err" role="alert">${ic("alert")}${t("upd.failed_hint")}</div>` : "";
  const state = t(`upd.state.${up.state}`);
  const icon = BUSY.includes(up.state) && up.state !== "waiting" ? ic("arc", "spin") : ic(up.state === "idle" || up.state === "ready" ? "dot" : "ring");
  const latest = up.latest ? `<dt>${t("upd.latest")}</dt><dd>${esc(up.latest)}</dd>` : "";
  return `<section class="sec" aria-labelledby="updTitle"><div><h2 id="updTitle">${t("upd.title")}</h2><p class="hint">${t("upd.hint")}</p></div>
    <dl class="kv"><dt>${t("upd.version")}</dt><dd>${esc(up.current)}</dd>${latest}<dt>${t("upd.last")}</dt><dd>${esc(lastCheck())}</dd></dl>
    <div class="inline"><span class="lbl">${t("upd.status")}</span><span class="badge ${BADGE[up.state]}" role="status">${icon}${state}</span></div>
    ${progress()}${failed}
    <div class="inline"><button class="btn" data-act="upd-check" ${busy ? "disabled" : ""}>${ic("refresh")}${t("upd.check")}</button>${apply}</div>
    <div class="divider"></div>
    <div class="split"><div><h3 class="h3">${t("upd.auto")}</h3><p class="hint">${t("upd.auto_hint")}</p></div>
      <label class="sw"><input type="checkbox" id="autoUpdate" role="switch" ${S.settings.auto_update ? "checked" : ""}><span class="tr"></span><span>${t(S.settings.auto_update ? "upd.on" : "upd.off")}</span></label></div></section>`;
}

export const updateActions = {
  "upd-check": () => invoke("update_check"),
  "upd-apply": () => invoke("update_apply"),
};

export async function startUpdate() {
  up = await invoke("update_state");
  await window.__TAURI__.event.listen("update://status", (e) => {
    up = e.payload;
    if (S.screen === "settings" && !document.activeElement?.closest?.("[role=tablist]")) render();
  });
}
