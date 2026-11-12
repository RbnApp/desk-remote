// Mouse and keyboard capture on the viewer canvas, normalised to 0..65535.

const RANGE = 65535;

/**
 * Viewport rectangle of the visible (letterboxed) picture. The canvas has a CSS border, so the
 * picture lives in the content box (client*), not the border box getBoundingClientRect() gives;
 * using the border box shifted every click by the border width.
 */
export function pictureRect(canvas) {
  const rect = canvas.getBoundingClientRect();
  const boxLeft = rect.left + canvas.clientLeft;
  const boxTop = rect.top + canvas.clientTop;
  const boxW = canvas.clientWidth;
  const boxH = canvas.clientHeight;
  const ratio = canvas.width / canvas.height;
  let w = boxW;
  let h = boxH;
  if (w / h > ratio) w = h * ratio;
  else h = w / ratio;
  return { left: boxLeft + (boxW - w) / 2, top: boxTop + (boxH - h) / 2, width: w, height: h };
}

/** Maps a pointer position to 0..65535 inside the visible (letterboxed) picture. */
export function normalize(canvas, clientX, clientY) {
  const { left, top, width: w, height: h } = pictureRect(canvas);
  const unit = (value, start, size) => Math.round(Math.min(1, Math.max(0, (value - start) / size)) * RANGE);
  return { x: unit(clientX, left, w), y: unit(clientY, top, h) };
}

/**
 * Wires pointer/wheel/keyboard listeners on `canvas`.
 * `send(event)` receives `InputEvent`-shaped objects; `isActive()` gates everything.
 * Returns `release()`: key-ups for every key still held, so a shortcut that moves focus away
 * (Ctrl+Tab) cannot leave a modifier stuck down on the remote PC. It also runs on blur.
 */
export function attachInput(canvas, send, isActive) {
  let pendingMove = null;
  const flush = () => {
    if (pendingMove) send(pendingMove);
    pendingMove = null;
  };
  const point = (e) => normalize(canvas, e.clientX, e.clientY);

  canvas.addEventListener("pointermove", (e) => {
    if (!isActive()) return;
    const first = pendingMove === null;
    pendingMove = { kind: "mouse_move", ...point(e) };
    if (first) requestAnimationFrame(flush);
  });
  const button = (pressed) => (e) => {
    if (!isActive()) return;
    e.preventDefault();
    canvas.focus();
    if (pressed) canvas.setPointerCapture(e.pointerId);
    flush();
    send({ kind: "mouse_button", button: e.button, pressed, ...point(e) });
  };
  canvas.addEventListener("pointerdown", button(true));
  canvas.addEventListener("pointerup", button(false));
  canvas.addEventListener("contextmenu", (e) => {
    if (isActive()) e.preventDefault();
  });
  canvas.addEventListener(
    "wheel",
    (e) => {
      if (!isActive()) return;
      e.preventDefault();
      send({ kind: "wheel", dx: Math.round(e.deltaX), dy: Math.round(e.deltaY) });
    },
    { passive: false },
  );
  const held = new Set();
  const key = (pressed) => (e) => {
    if (!isActive()) return;
    e.preventDefault();
    if (pressed) held.add(e.code);
    else held.delete(e.code);
    send({ kind: "key", code: e.code, pressed });
  };
  canvas.addEventListener("keydown", key(true));
  canvas.addEventListener("keyup", key(false));
  const release = () => {
    for (const code of held) send({ kind: "key", code, pressed: false });
    held.clear();
  };
  canvas.addEventListener("blur", release);
  return release;
}
