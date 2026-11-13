// Right-click handling (DAPP-114): the browser's own menu never shows in the panel; text fields keep
// theirs (copy / paste), and a PC card or row gets a small menu of its own.

import { S, esc, ic, t } from "./core.js";

const TEXT_FIELD = 'input:not([type="radio"]):not([type="checkbox"]):not([type="button"]), textarea, [contenteditable="true"]';
let menu = null;

function closeMenu() {
  if (!menu) return;
  const opener = menu.opener;
  menu.el.remove();
  menu = null;
  if (opener?.isConnected) opener.focus({ preventScroll: true });
}

function items(pc) {
  const id = esc(pc.id);
  const canConnect = pc.status === "online" || pc.status === "warning";
  const connect = canConnect
    ? `<button role="menuitem" data-act="connect" data-id="${id}">${ic("link")}${t("list.connect")}</button>`
    : `<button role="menuitem" aria-disabled="true" data-act="noop">${ic("power")}${t("list.unavailable")}</button>`;
  return `${connect}<button role="menuitem" data-act="rename" data-id="${id}">${ic("edit")}${t("list.rename")}</button>
    <button role="menuitem" class="danger" data-act="unpair" data-id="${id}">${ic("x")}${t("list.unpair")}</button>`;
}

function openMenu(pc, x, y) {
  closeMenu();
  const el = document.createElement("div");
  el.className = "ctx-menu";
  el.setAttribute("role", "menu");
  el.setAttribute("aria-label", pc.name);
  el.innerHTML = items(pc);
  document.body.append(el);
  const { width, height } = el.getBoundingClientRect();
  el.style.left = `${Math.max(0, Math.min(x, window.innerWidth - width))}px`;
  el.style.top = `${Math.max(0, Math.min(y, window.innerHeight - height))}px`;
  menu = { el, opener: document.activeElement };
  el.querySelector("button")?.focus({ preventScroll: true });
}

document.addEventListener("contextmenu", (e) => {
  if (e.target.closest(TEXT_FIELD)) return; // copy / paste stays available in text boxes
  e.preventDefault();
  if (S.screen !== "list") return closeMenu();
  const host = e.target.closest("[data-pc]");
  const pc = host && S.pcs.find((p) => p.id === host.dataset.pc);
  if (pc) openMenu(pc, e.clientX, e.clientY);
  else closeMenu();
});

// Any click (a menu entry's own action runs first) or scroll/resize/blur closes it.
document.addEventListener("click", () => closeMenu());
window.addEventListener("blur", closeMenu);
window.addEventListener("resize", closeMenu);
document.addEventListener("scroll", closeMenu, true);

document.addEventListener("keydown", (e) => {
  if (!menu) return;
  const buttons = [...menu.el.querySelectorAll("button")];
  const at = buttons.indexOf(document.activeElement);
  if (e.key === "Escape") {
    e.preventDefault();
    closeMenu();
  } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    e.preventDefault();
    const step = e.key === "ArrowDown" ? 1 : -1;
    buttons[(at + step + buttons.length) % buttons.length].focus();
  } else if (e.key === "Tab") {
    closeMenu();
  }
});
