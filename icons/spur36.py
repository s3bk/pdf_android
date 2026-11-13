#!/usr/bin/env python3
"""Ø36 external-spur variant of the pdf-rs viewer icon, next to the ring.

Same gear numbers as the ring (N=16, m=2 mm, alpha=25 deg, backlash 0), but a
straight EXTERNAL spur: tips at dia 36.0 mm, root/base at dia 29.0 mm, so the
teeth stand 3.5 mm proud of the body.  Scaled so the TIPS touch the launcher
circle (r 238): the valleys between the teeth are the app's own slate, so the
icon edge is scalloped rather than a clean circle.

The white bore is pulled in from the root circle so a body ring of rust is left
between the bore and the toothing (BORE_FRAC of the root radius); the word then
fits inside the bore as before.

Writes icon-spur36.svg (+ masked preview) and spur.html, a side-by-side strip
against the current ring at 256/144/72/48 px.
"""
import math
import os

import gear as G
import draw_icon as D

HERE = os.path.dirname(os.path.abspath(__file__))
MASK_R = D.MASK_R
CX = CY = D.CX
BORE_FRAC = 0.84        # bore radius as a fraction of the root radius

g = G.Gear(16, 2.0, 25.0, 0.0, kind="external")
g.verify()

R_TIP = g.r_tip          # 18.000 mm
R_ROOT = g.r_root        # 14.501 mm (the base circle: no involute below it)
SCALE = MASK_R / R_TIP   # tips on the launcher circle


def canvas_pts(outline, scale):
    out = []
    for i, (r, th) in enumerate(outline):
        x = CX + r * scale * math.cos(th)
        y = CY + r * scale * math.sin(th)
        out.append(("M" if i == 0 else "L") + "{:.2f},{:.2f}".format(x, y))
    return "".join(out) + "Z"


def word_for(fit_r):
    a = fit_r / math.hypot(1.0, (D.H_CAP / D.WORD_W))
    ws = 2.0 * a / D.WORD_W
    return D.scale_path(D.WORD, ws), ws


TEETH = canvas_pts(g.outline(), SCALE)
BORE_R = R_ROOT * SCALE * BORE_FRAC
WORD, WORD_SCALE = word_for(BORE_R - 16.0)

SHAPES = [dict(d=TEETH, fill=D.RUST),
          dict(circle=BORE_R, fill=D.PAPER),
          dict(d=WORD, fill=D.BG, **{"fill-rule": "nonzero"})]


def write(path, text):
    with open(os.path.join(HERE, path), "w") as f:
        f.write(text)
    print("wrote", path, len(text), "B")


HTML = """<!doctype html>
<meta charset="utf-8"><title>dia 36 spur vs ring</title>
<style>
 body {{ margin:0; background:#f2f4f7; font:12px/1.4 system-ui,sans-serif;
        color:#3b444e; display:flex; gap:30px; padding:24px; }}
 figure {{ margin:0; text-align:center; }} figcaption {{ margin-top:6px }}
 img {{ display:block; border-radius:22.4% }}
 .row {{ display:flex; gap:18px; align-items:flex-end }}
 h3 {{ margin:0 0 10px; font:600 13px system-ui,sans-serif; }}
</style>
<div>
 <h3>Ø36 external spur</h3>
 <div class="row">
  <figure><img src="icon-spur36.svg?v={v}" width="256" height="256"><figcaption>256</figcaption></figure>
  <figure><img src="icon-spur36.svg?v={v}" width="144" height="144"><figcaption>144</figcaption></figure>
  <figure><img src="icon-spur36.svg?v={v}" width="72" height="72"><figcaption>72</figcaption></figure>
  <figure><img src="icon-spur36.svg?v={v}" width="48" height="48"><figcaption>48</figcaption></figure>
 </div>
</div>
<div>
 <h3>current: Ø41 internal ring</h3>
 <div class="row">
  <figure><img src="icon.svg?v={v}" width="256" height="256"><figcaption>256</figcaption></figure>
  <figure><img src="icon.svg?v={v}" width="144" height="144"><figcaption>144</figcaption></figure>
  <figure><img src="icon.svg?v={v}" width="72" height="72"><figcaption>72</figcaption></figure>
  <figure><img src="icon.svg?v={v}" width="48" height="48"><figcaption>48</figcaption></figure>
 </div>
</div>
"""

if __name__ == "__main__":
    write("icon-spur36.svg", D.svg(SHAPES, bg=D.BG))
    print("  tips r {:.1f}, valleys r {:.1f}, bore r {:.1f} (frac {:.2f}), "
          "word {:.1f} x {:.1f}".format(R_TIP * SCALE, R_ROOT * SCALE, BORE_R,
                                        BORE_FRAC, D.WORD_W * WORD_SCALE,
                                        D.H_CAP * WORD_SCALE))
    write("preview-spur36.svg", D.svg_masked(SHAPES))
    v = "65"
    write("spur.html", HTML.format(v=v))
