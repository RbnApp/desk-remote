// Reusable UI components. Each repeated element is defined exactly once, here.

import { S, esc, ic, t } from "./core.js";
import { inc as incoming } from "./incoming.js";

const UNKNOWN_MINUTES = 4294967295;

export function Badge(status) {
  const shape = { online: "dot", offline: "ring", connecting: "arc", warning: "tri" }[status];
  const spin = status === "connecting" ? "spin" : "";
  return `<span class="badge ${status}"><svg class="${spin}" aria-hidden="true"><use href="#i-${shape}"/></svg>${t(`status.${status}`)}</span>`;
}

export function lastSeen(pc) {
  if (pc.last_seen_min === UNKNOWN_MINUTES) return "—";
  if (pc.status === "online" || pc.status === "connecting" || (pc.last_seen_min < 1 && pc.status !== "warning")) {
    return t("last.now");
  }
  if (pc.last_seen_min < 60) return t("last.min", { n: pc.last_seen_min });
  if (pc.last_seen_min < 1440) return t("last.hour", { n: Math.round(pc.last_seen_min / 60) });
  return t("last.day", { n: Math.round(pc.last_seen_min / 1440) });
}

/** Segmented radio control. `options`: [{v, l, icon?}]. */
export function Seg(name, label, options, current) {
  const items = options
    .map(
      (o) =>
        `<label><input type="radio" name="${name}" value="${o.v}" ${o.v === current ? "checked" : ""}><span>${o.icon ? ic(o.icon) : ""}${esc(o.l)}</span></label>`,
    )
    .join("");
  return `<div class="seg" role="radiogroup" aria-label="${esc(label)}">${items}</div>`;
}

export function Thumb(pc) {
  if (pc.status === "offline") {
    return `<div class="thumb off" aria-hidden="true"><div class="offmsg">${ic("wifi-off")}${t("list.offline_thumb")}</div></div>`;
  }
  if (pc.status === "connecting") return `<div class="thumb load" aria-hidden="true"></div>`;
  return `<div class="thumb" aria-hidden="true"><div class="mini"><i></i><i></i><i></i></div></div>`;
}

export function ConnectBtn(pc, small) {
  const cls = small ? "btn sm" : "btn";
  if (pc.status === "offline") {
    return `<button class="${cls}" aria-disabled="true" title="${esc(t("list.unavailable_hint"))}" data-act="noop">${ic("power")}${t("list.unavailable")}</button>`;
  }
  if (pc.status === "connecting") {
    return `<button class="${cls}" aria-disabled="true" data-act="noop">${ic("arc", "spin")}${t("list.connecting")}</button>`;
  }
  return `<button class="${cls} primary" data-act="connect" data-id="${esc(pc.id)}" aria-label="${esc(`${t("list.connect")} — ${pc.name}`)}">${ic("link")}${t("list.connect")}</button>`;
}

/** "Send file" button on a PC; only reachable PCs get one. */
export function SendBtn(pc, small) {
  if (pc.status !== "online" && pc.status !== "warning") return "";
  const label = `${t("transfer.send")} — ${pc.name}`;
  if (small) return `<button class="btn sm icon" data-act="tx-card" data-id="${esc(pc.id)}" title="${esc(t("transfer.send"))}" aria-label="${esc(label)}">${ic("send")}</button>`;
  return `<button class="btn" data-act="tx-card" data-id="${esc(pc.id)}" aria-label="${esc(label)}">${ic("send")}${t("transfer.send")}</button>`;
}

/** Small "Remove pairing" button (DAPP-104); works for offline PCs too. */
export function UnpairBtn(pc) {
  return `<button class="btn sm ghost" data-act="unpair" data-id="${esc(pc.id)}" aria-label="${esc(`${t("list.unpair")} — ${pc.name}`)}">${ic("x")}${t("list.unpair")}</button>`;
}

const warnText = (pc) => (pc.warn ? t(`warn.${pc.warn}`) : "");

const latency = (pc) => (pc.status === "online" || pc.status === "warning") && pc.latency_ms != null ? ` · ${pc.latency_ms} ms` : "";

export function PcCard(pc) {
  const seen = `<div class="meta">${ic("clock")}${t("last.label")}: ${lastSeen(pc)}${latency(pc)}</div>`;
  const warn = pc.warn ? `<div class="meta w">${ic("alert")}${warnText(pc)}</div>` : "";
  return `<article class="card" aria-label="${esc(pc.name)}">${Thumb(pc)}<div class="body"><div class="top"><div class="name" title="${esc(pc.name)}">${esc(pc.name)}</div>${Badge(pc.status)}</div>${warn}${seen}<div class="act">${ConnectBtn(pc)}${SendBtn(pc)}${UnpairBtn(pc)}</div></div></article>`;
}

export function PcRow(pc) {
  const warn = pc.warn ? `<div class="sub2" style="color:var(--warning)">${warnText(pc)}</div>` : "";
  return `<tr><td><div class="name" title="${esc(pc.name)}">${esc(pc.name)}</div>${warn}</td><td>${Badge(pc.status)}</td><td class="sub2">${lastSeen(pc)}${latency(pc)}</td><td class="act"><div class="btn-group">${SendBtn(pc, true)}${ConnectBtn(pc, true)}${UnpairBtn(pc)}</div></td></tr>`;
}

export function StateBlock(icon, title, body, buttons) {
  return `<div class="state"><div class="ico">${ic(icon)}</div><h2>${title}</h2><p>${body}</p><div class="row">${buttons}</div></div>`;
}

/** Sidebar "Connection" box: local network and Tailscale. */
export function NetStatus() {
  const ts = S.tailscale;
  let shape = "ring";
  let cls = "offline";
  let label = t("net.off");
  if (!ts.checked) {
    label = t("net.checking");
  } else if (ts.state === "running") {
    shape = "dot";
    cls = "online";
    label = t("net.on");
  } else if (ts.state === "starting") {
    label = t("net.starting");
  } else if (ts.state === "needs_login") {
    label = t("net.needs_login");
  } else if (ts.state === "failed") {
    label = t("net.failed");
  }
  return `<div class="side-foot" aria-label="${t("net.label")}"><span>${t("net.label")}</span>
    <div class="net-row"><span>${t("net.lan")}</span><span class="badge online">${ic("dot")}${t("net.lan_ok")}</span></div>
    <div class="net-row"><span>${t("net.tailscale")}</span><span class="badge ${cls}">${ic(shape)}${label}</span></div></div>`;
}

/** Page frame shared by the list, pairing and settings screens. */
export function Shell(inner, active, head) {
  const nav = (id, icon, label, extra = "") =>
    `<button data-go="${id}" ${active === id || (id === "list" && active === "pair") ? 'aria-current="page"' : ""}>${ic(icon)}<span>${label}</span>${extra}</button>`;
  const narrow = active === "settings" || active === "pair" || active === "incoming" ? "narrow" : "";
  return `<aside class="side"><div class="brand"><div class="logo">${ic("logo")}</div><span>${t("app.name")}</span></div>
    <nav class="nav" aria-label="${t("nav.main")}">${nav("list", "grid", t("nav.pcs"), `<span class="count">${S.pcs.length}</span>`)}${nav("incoming", "monitor", t("incoming.nav"), incoming.enabled ? `<span class="count">${t("incoming.on_short")}</span>` : "")}${nav("settings", "settings", t("nav.settings"))}</nav>
    ${NetStatus()}</aside>
    <main class="main">${head}<div class="content ${narrow}" id="content">${inner}</div></main>`;
}
