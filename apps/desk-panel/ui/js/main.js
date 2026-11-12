// Entry point: loads state from Rust, wires global events, shows the window.

import { S, appWindow, go, invoke, refocus, render } from "./core.js";
import { applyTheme, loadCatalog, loadPreviews, watchSystemTheme } from "./theme.js";
import { loadPcs, refreshResults } from "./list.js";
import { incomingActions, setIncomingEnabled, startIncoming } from "./incoming.js";
import { startTransfer, transferActions } from "./transfer.js";
import { onCodeInput, onTargetInput, pair, pickTarget, resetPair, submitPair } from "./pair.js";
import {
  connectTailscale,
  copyKeyCommand,
  onFarmKeyInput,
  onSettingChange,
  openInstallPage,
  openKeysPage,
  saveSettings,
  startSidebarPolling,
} from "./settings.js";
import {
  armHide,
  closeViewer,
  dropDemoConnection,
  openViewer,
  retryNow,
  setControl,
  toggleFullscreen,
  viewerState,
} from "./viewer.js";

const actions = {
  connect: (el) => openViewer(el.dataset.id),
  clear: () => {
    S.filter = "all";
    S.query = "";
    render();
  },
  retry: () => loadPcs(),
  disconnect: () => closeViewer(),
  fs: () => toggleFullscreen(),
  "v-retry": () => retryNow(),
  "demo-drop": () => dropDemoConnection(),
  "pick-target": (el) => pickTarget(el.dataset.address),
  "pair-again": () => {
    resetPair();
    render();
  },
  "ts-connect": () => connectTailscale(),
  "ts-keys": () => openKeysPage(),
  "ts-install": () => openInstallPage(),
  "ts-copy": () => copyKeyCommand(),
  ...incomingActions,
  ...transferActions,
};

document.addEventListener("click", (e) => {
  const el = e.target.closest("[data-go],[data-act],[data-filter]");
  if (!el) return;
  if (el.dataset.go) {
    if (el.dataset.go === "pair" && pair.phase === "success") resetPair();
    go(el.dataset.go);
  } else if (el.dataset.filter) {
    S.filter = el.dataset.filter;
    render();
  } else {
    actions[el.dataset.act]?.(el);
  }
});

document.addEventListener("change", async (e) => {
  const { name, id, value, checked } = e.target;
  if (name === "view") {
    await saveSettings({ list_view: value });
    render();
    refocus('input[name="view"]:checked');
  } else if (name === "palette" || name === "mode" || name === "lang") {
    await onSettingChange(name, value);
  } else if (id === "incEnabled") {
    setIncomingEnabled(checked);
  } else if (id === "ctl") {
    setControl(checked);
  }
});

document.addEventListener("input", (e) => {
  const { id, value } = e.target;
  if (id === "q") {
    S.query = value;
    refreshResults();
  } else if (id === "code") {
    onCodeInput(e.target);
  } else if (id === "target") {
    onTargetInput(value);
  } else if (id === "pname") {
    pair.name = value;
  } else if (id === "farmKey") {
    onFarmKeyInput(value);
  }
});

document.addEventListener("submit", (e) => {
  if (e.target.id !== "pairForm") return;
  e.preventDefault();
  submitPair();
});

document.addEventListener("mousemove", () => {
  if (viewerState.fs && S.screen === "viewer") armHide();
});

document.addEventListener("keydown", (e) => {
  if (S.screen !== "viewer") return;
  armHide();
  if (e.key === "Escape" && viewerState.fs && !viewerState.control) toggleFullscreen();
});

async function boot() {
  try {
    S.info = await invoke("app_info");
    S.settings = await invoke("get_settings");
    await Promise.all([loadCatalog(), loadPreviews(), applyTheme()]);
    watchSystemTheme();
    render();
    loadPcs();
    startSidebarPolling();
    startIncoming();
    startTransfer();
  } finally {
    await appWindow().show();
  }
}

boot();
