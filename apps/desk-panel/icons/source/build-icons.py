"""Desk-Remote icon set builder (DAPP-76, direction A "Ekran Duvari").
Needs Chrome/Edge (headless raster) + Pillow. Run from repo root:
  python -I apps/desk-panel/icons/source/build-icons.py
Source of truth = the SVG strings below; colours come from design/tokens/themes.json
(ink #c2185b/#f472b6, petrol #0b7a75/#2dd4bf, purple #6d4fd6/#c4b5fd). All rasters are derived."""
import os, struct, subprocess, tempfile, shutil

ROOT = os.getcwd()
PANEL = os.path.join(ROOT, "apps/desk-panel/icons")
AGENT = os.path.join(ROOT, "apps/desk-session-helper/icons")
CHROME = next(p for p in (r"C:\Program Files\Google\Chrome\Application\chrome.exe",
                          r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe") if os.path.exists(p))

DEFS = ('<defs><linearGradient id="bg" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#2b2a27"/><stop offset="1" stop-color="#0f0f0e"/></linearGradient>'
 '<linearGradient id="rim" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff" stop-opacity=".28"/><stop offset=".5" stop-color="#fff" stop-opacity=".04"/><stop offset="1" stop-color="#fff" stop-opacity=".1"/></linearGradient>'
 '<linearGradient id="t" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#2dd4bf"/><stop offset="1" stop-color="#0b7a75"/></linearGradient>'
 '<linearGradient id="m" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#f472b6"/><stop offset="1" stop-color="#c2185b"/></linearGradient>'
 '<linearGradient id="p" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#c4b5fd"/><stop offset="1" stop-color="#6d4fd6"/></linearGradient>'
 '<filter id="sh" x="-20%" y="-20%" width="140%" height="150%"><feDropShadow dx="0" dy="5" stdDeviation="6" flood-color="#000" flood-opacity=".45"/></filter></defs>'
 '<rect x="8" y="8" width="240" height="240" rx="56" fill="url(#bg)"/><rect x="9" y="9" width="238" height="238" rx="55" fill="none" stroke="url(#rim)" stroke-width="2"/>')


def svg(body):
    return '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256">' + DEFS + '<g filter="url(#sh)">' + body + '</g></svg>'


PANEL_TILES = ('<rect x="40" y="48" width="76" height="58" rx="14" fill="url(#t)"/><rect x="124" y="48" width="76" height="58" rx="14" fill="url(#m)"/>'
               '<rect x="40" y="114" width="76" height="58" rx="14" fill="url(#p)"/><rect x="124" y="114" width="76" height="58" rx="14" fill="url(#t)"/>')
PANEL_BARS = ''.join(
    f'<rect x="{x}" y="{y}" width="26" height="6" rx="3" fill="#fff" fill-opacity=".55"/><rect x="{x}" y="{y + 12}" width="40" height="4" rx="2" fill="#fff" fill-opacity=".25"/>'
    for x, y in ((50, 59), (134, 59), (50, 125), (134, 125)))
LENS = ('<circle cx="186" cy="182" r="38" fill="#181816"/><circle cx="186" cy="182" r="38" fill="none" stroke="url(#t)" stroke-width="10"/>'
        '<circle cx="186" cy="182" r="14" fill="url(#m)"/>')
PANEL_FULL = svg(PANEL_TILES + PANEL_BARS + LENS + '<circle cx="181" cy="177" r="4" fill="#fff" fill-opacity=".8"/>')
PANEL_SMALL = svg(PANEL_TILES + LENS)

WAVES = ("M146.0 82.6 A28 28 0 0 1 146.0 125.4", "M110.0 125.4 A28 28 0 0 1 110.0 82.6",
         "M157.6 68.8 A46 46 0 0 1 157.6 139.2", "M98.4 139.2 A46 46 0 0 1 98.4 68.8")


def waves(w, color="url(#m)"):
    return ''.join(f'<path d="{d}" fill="none" stroke="{color}" stroke-width="{w}" stroke-linecap="round"/>' for d in WAVES)


AGENT_BASE = ('<rect x="48" y="46" width="160" height="116" rx="22" fill="url(#t)"/><rect x="60" y="58" width="136" height="92" rx="12" fill="#181816"/>'
              '<rect x="118" y="160" width="20" height="22" fill="url(#p)"/><rect x="84" y="178" width="88" height="14" rx="7" fill="url(#p)"/>'
              '<circle cx="128" cy="104" r="11" fill="url(#m)"/>')
AGENT_FULL = svg(AGENT_BASE + waves(8))
AGENT_SMALL = svg(AGENT_BASE + waves(11))


def mono(inner, vb, fill):
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{vb}"><defs><mask id="k" maskUnits="userSpaceOnUse" x="0" y="0" width="256" height="256">{inner}</mask></defs>'
            f'<rect x="0" y="0" width="256" height="256" fill="{fill}" mask="url(#k)"/></svg>')


AGENT_MONO = ('<rect x="48" y="46" width="160" height="116" rx="22" fill="#fff"/><rect x="62" y="60" width="132" height="88" rx="10" fill="#000"/>'
              '<circle cx="128" cy="104" r="11" fill="#fff"/>' + waves(11, "#fff") +
              '<rect x="118" y="160" width="20" height="22" fill="#fff"/><rect x="84" y="178" width="88" height="14" rx="7" fill="#fff"/>')


def wr(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    if isinstance(data, bytes):
        open(path, "wb").write(data)
    else:
        open(path, "w", encoding="utf-8", newline="\n").write(data)


tmp = tempfile.mkdtemp()


def raster(svgtext, size, out):
    html = os.path.join(tmp, "r.html")
    wr(html, f'<!doctype html><meta charset="utf-8"><style>html,body{{margin:0;background:transparent}}svg{{display:block;width:{size}px;height:{size}px}}</style>' + svgtext)
    os.makedirs(os.path.dirname(out), exist_ok=True)
    subprocess.run([CHROME, "--headless=new", "--disable-gpu", "--hide-scrollbars", "--force-device-scale-factor=1",
                    "--default-background-color=00000000", f"--window-size={size},{size}", f"--screenshot={out}",
                    "file:///" + html.replace("\\", "/")], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    from PIL import Image
    im = Image.open(out).convert("RGBA")
    if im.size != (size, size):  # headless chrome has a minimum window width; crop to exact
        im = im.crop((0, 0, size, size))
    im.save(out)


def ico(full, small, sizes, out):
    blobs = []
    for s in sizes:
        p = os.path.join(tmp, f"i{s}.png")
        raster(small if s <= 32 else full, s, p)
        blobs.append((s, open(p, "rb").read()))
    head = struct.pack("<HHH", 0, 1, len(blobs))
    off = 6 + 16 * len(blobs)
    ent, data = b"", b""
    for s, b in blobs:
        ent += struct.pack("<BBBBHHII", 0 if s >= 256 else s, 0 if s >= 256 else s, 0, 0, 1, 32, len(b), off + len(data))
        data += b
    wr(out, head + ent + data)


SIZES = (16, 24, 32, 48, 64, 256)
# --- Panel
wr(f"{PANEL}/source/desk-remote.svg", PANEL_FULL)
wr(f"{PANEL}/source/desk-remote-small.svg", PANEL_SMALL)
ico(PANEL_FULL, PANEL_SMALL, SIZES, f"{PANEL}/icon.ico")
raster(PANEL_FULL, 512, f"{PANEL}/icon.png")
raster(PANEL_FULL, 128, f"{PANEL}/128x128.png")
raster(PANEL_FULL, 256, f"{PANEL}/128x128@2x.png")
raster(PANEL_SMALL, 32, f"{PANEL}/32x32.png")
# --- Agent
wr(f"{AGENT}/source/desk-agent.svg", AGENT_FULL)
wr(f"{AGENT}/source/desk-agent-small.svg", AGENT_SMALL)
ico(AGENT_FULL, AGENT_SMALL, SIZES, f"{AGENT}/icon.ico")
raster(AGENT_FULL, 512, f"{AGENT}/icon.png")
# --- Tray: file names kept (tray-dark = dark glyph for light taskbar, tray-light = light glyph for dark taskbar). Agent monochrome.
for name, fill in (("dark", "#181816"), ("light", "#f3f2ee")):
    t = mono(AGENT_MONO, "40 36 176 176", fill)
    wr(f"{PANEL}/tray/tray-{name}.svg", t)
    for s in (16, 32):
        raster(t, s, f"{PANEL}/tray/tray-{name}-{s}.png")
shutil.rmtree(tmp, ignore_errors=True)
print("ok")
