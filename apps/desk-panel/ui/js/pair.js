// PC add / pairing screen.

import { S, $, esc, ic, invoke, mountApp, screens, t } from "./core.js";
import { Shell } from "./components.js";
import { loadPcs } from "./list.js";

const KNOWN_ERRORS = ["invalid", "expired", "used", "network"];
const CODE_LENGTH = 8;

export const pair = { code: "", name: "", phase: "idle", err: null, pcName: "", newId: null };

export function resetPair() {
  Object.assign(pair, { code: "", name: "", phase: "idle", err: null, pcName: "", newId: null });
}

const codeComplete = () => pair.code.replace("-", "").length === CODE_LENGTH;

/** Normalises typed input to `ABCD-2345`. */
export function formatCode(raw) {
  const clean = raw.toUpperCase().replace(/[^A-Z0-9]/g, "").slice(0, CODE_LENGTH);
  return clean.length > 4 ? `${clean.slice(0, 4)}-${clean.slice(4)}` : clean;
}

function successBox() {
  return `<div class="panel"><div class="success-box" role="status"><div class="ico">${ic("check")}</div><h2>${t("pair.ok.title")}</h2><p class="hint">${t("pair.ok.body", { name: `<b>${esc(pair.pcName)}</b>` })}</p>
    <div class="inline" style="margin-top:8px"><button class="btn primary" data-act="connect" data-id="${esc(pair.newId)}">${ic("link")}${t("pair.ok.connect")}</button><button class="btn" data-act="pair-again">${t("pair.ok.another")}</button><button class="btn ghost" data-go="list">${t("pair.back")}</button></div></div></div>`;
}

function errorBox() {
  if (!pair.err) return "";
  return `<div class="msg err">${ic("alert")}<div><b>${t(`pair.err.${pair.err}.title`)}</b><br>${t(`pair.err.${pair.err}.detail`)}</div></div>`;
}

function form() {
  const busy = pair.phase === "verifying";
  const submit = busy
    ? `${ic("arc", "spin")}${t("pair.verifying")}`
    : `${ic("check")}${t("pair.submit")}`;
  const disabled = !codeComplete() || busy ? 'aria-disabled="true" disabled' : "";
  const demoHint = S.info.demo ? `<div class="hint">${t("pair.demo")}</div>` : "";
  return `<form class="panel" id="pairForm" novalidate><h2>${t("pair.form.title")}</h2>
    <div class="field"><label for="code">${t("pair.code.label")}</label><input id="code" class="input code" inputmode="text" autocomplete="off" spellcheck="false" maxlength="9" placeholder="XXXX-XXXX" value="${esc(pair.code)}" aria-describedby="codeHint codeErr" ${pair.err ? 'aria-invalid="true"' : ""} ${busy ? "readonly" : ""}><div class="hint" id="codeHint">${t("pair.code.hint")}</div>
    <div id="codeErr" role="alert">${errorBox()}</div></div>
    <div class="field"><label for="pname">${t("pair.name.label")}</label><input id="pname" class="input" placeholder="${esc(t("pair.name.ph"))}" value="${esc(pair.name)}" autocomplete="off" aria-describedby="pnameHint" ${busy ? "readonly" : ""}><div class="hint" id="pnameHint">${t("pair.name.hint")}</div></div>
    <div class="actions"><button type="button" class="btn" data-go="list">${t("pair.back")}</button><button type="submit" class="btn primary" id="pairBtn" ${disabled}>${submit}</button></div>${demoHint}</form>`;
}

function steps() {
  const items = [1, 2, 3]
    .map((n) => `<li><div><b>${t(`pair.how.step${n}.title`)}</b>${t(`pair.how.step${n}.body`)}</div></li>`)
    .join("");
  return `<aside class="panel"><h2>${t("pair.how.title")}</h2><ol class="steps">${items}</ol></aside>`;
}

function renderPair() {
  const head = `<header class="head"><button class="btn icon ghost" data-go="list" aria-label="${t("pair.back")}">${ic("back")}</button><div class="grow"><h1>${t("pair.title")}</h1><div class="sub">${t("pair.sub")}</div></div></header>`;
  const main = pair.phase === "success" ? successBox() : form();
  mountApp(Shell(`<div class="pair" style="max-width:none">${main}${steps()}</div>`, "pair", head));
  const input = $("#code");
  if (input && pair.phase === "idle") input.focus();
}

/** Live formatting of the code field without re-rendering the form. */
export function onCodeInput(input) {
  input.value = formatCode(input.value);
  pair.code = input.value;
  pair.err = null;
  input.removeAttribute("aria-invalid");
  $("#codeErr").innerHTML = "";
  const button = $("#pairBtn");
  button.disabled = !codeComplete();
  if (codeComplete()) button.removeAttribute("aria-disabled");
  else button.setAttribute("aria-disabled", "true");
}

export async function submitPair() {
  if (!codeComplete() || pair.phase === "verifying") return;
  pair.phase = "verifying";
  pair.err = null;
  renderPair();
  try {
    const pc = await invoke("pair_pc", { code: pair.code, name: pair.name.trim() || null });
    pair.phase = "success";
    pair.pcName = pc.name;
    pair.newId = pc.id;
    await loadPcs();
  } catch (code) {
    pair.phase = "idle";
    pair.err = KNOWN_ERRORS.includes(code) ? code : "network";
  }
  if (S.screen === "pair") {
    renderPair();
    if (pair.err) {
      const input = $("#code");
      input.focus();
      input.setSelectionRange(input.value.length, input.value.length);
    }
  }
}

screens.pair = renderPair;
