// Remote mouse arrow: the agent sends the cursor shape and position separately (desktop capture
// never contains it); this is the only place that draws it, on an overlay above the picture.

import { pictureRect } from "./input.js";

const RANGE = 65535;

export class RemoteCursor {
  constructor(screen, canvas) {
    this.screen = screen;
    this.canvas = canvas;
    this.shapes = new Map();
    this.pos = null;
    this.el = document.createElement("canvas");
    this.el.className = "rcursor";
    this.el.setAttribute("aria-hidden", "true");
    screen.appendChild(this.el);
    this.shown = null;
  }

  setShape(m) {
    const c = document.createElement("canvas");
    c.width = m.width;
    c.height = m.height;
    c.getContext("2d").putImageData(new ImageData(Uint8ClampedArray.from(m.rgba), m.width, m.height), 0, 0);
    this.shapes.set(m.id, { canvas: c, hotX: m.hotX, hotY: m.hotY, width: m.width, height: m.height });
    if (this.pos?.shape === m.id) this.place();
  }

  setPos(m) {
    this.pos = m;
    this.place();
  }

  /** Re-places the arrow, e.g. after the window was resized. */
  place() {
    const p = this.pos;
    const shape = p && this.shapes.get(p.shape);
    if (!p || !p.visible || !shape) {
      this.el.style.display = "none";
      return;
    }
    if (this.shown !== shape) {
      this.el.width = shape.width;
      this.el.height = shape.height;
      this.el.getContext("2d").drawImage(shape.canvas, 0, 0);
      this.shown = shape;
    }
    const pic = pictureRect(this.canvas);
    const origin = this.screen.getBoundingClientRect();
    const x = pic.left - origin.left + (p.x / RANGE) * pic.width - shape.hotX;
    const y = pic.top - origin.top + (p.y / RANGE) * pic.height - shape.hotY;
    this.el.style.display = "block";
    this.el.style.transform = `translate(${x}px, ${y}px)`;
  }

  close() {
    this.el.remove();
    this.shapes.clear();
  }
}
