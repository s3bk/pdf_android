#!/usr/bin/env python3
"""Turn the hand-drawn icons/icon.svg geometry into Android VectorDrawables.

The SVG in this directory is the source of truth (icons/icon.svg, plus
icon-fg.svg and icon-mono.svg for the adaptive / themed layers). Android cannot
read SVG at runtime, but a VectorDrawable is the same path data, so nothing has
to be rasterised and no typst is involved: this script writes the XML.

What it cannot express is stroke-dasharray, so the dashed "not fetched yet"
outline is emitted as explicit dash segments along the polyline.
"""
import math
import os

HERE = os.path.dirname(os.path.abspath(__file__))
RES = os.path.join(HERE, "..", "android", "res")

# --- geometry, identical to icon.svg ---------------------------------------
PAGE = "M152,96 H316 L360,140 V272 H152 Z"          # fetched part of the page
FOLD = "M316,96 L360,140 H316 Z"                     # folded corner
GHOST_FILL = "M152,272 H360 V416 H152 Z"             # still only a promise
GHOST_LINE = [(152, 272), (152, 416), (360, 416), (360, 272)]
BG = "#1b2b3a"
PAPER = "#ffffff"
FOLD_COLOR = "#b9c4d1"
GHOST_STROKE = "#7fb2ff"
SAFE = 0.8          # adaptive foreground: art inside the 66/108 safe circle
SAFE_OFF = 256 * (1 - SAFE)

HEAD = """<?xml version="1.0" encoding="utf-8"?>
<!-- Generated from icons/icon.svg by icons/make_icons.py - edit the SVG, then
     rerun the script. VectorDrawables have no stroke-dasharray, so the dashed
     edge below is a list of dash segments. -->
<vector xmlns:android="http://schemas.android.com/apk/res/android"
    android:width="{dp}dp"
    android:height="{dp}dp"
    android:viewportWidth="512"
    android:viewportHeight="512">"""

GROUP_OPEN = """
    <group android:translateX="{tx}" android:translateY="{ty}"
        android:scaleX="{s}" android:scaleY="{s}">"""
GROUP_CLOSE = """
    </group>"""


def dashes(points, on, off):
    """Path data of dash segments along a polyline, corners kept as vertices."""
    total = sum(math.hypot(x1 - x0, y1 - y0)
                for (x0, y0), (x1, y1) in zip(points, points[1:]))
    pieces, pos = [], 0.0
    while pos < total:
        end = min(pos + on, total)
        # collect the polyline vertices strictly between pos and end
        walked, part = 0.0, [point_at(points, pos)]
        for a, b in zip(points, points[1:]):
            walked += math.hypot(b[0] - a[0], b[1] - a[1])
            if pos < walked < end:
                part.append((float(b[0]), float(b[1])))
        part.append(point_at(points, end))
        pieces.append("M" + " L".join("{:.2f},{:.2f}".format(x, y) for x, y in part))
        pos += on + off
    return " ".join(pieces)


def point_at(points, d):
    for (x0, y0), (x1, y1) in zip(points, points[1:]):
        length = math.hypot(x1 - x0, y1 - y0)
        if d <= length:
            k = 0.0 if length == 0 else d / length
            return (x0 + (x1 - x0) * k, y0 + (y1 - y0) * k)
        d -= length
    return (float(points[-1][0]), float(points[-1][1]))


def body(indent="    ", paper=PAPER, fold=FOLD_COLOR, ghost_fill="0.14",
         ghost_stroke=GHOST_STROKE, dash=(16, 12), stroke=6, k=1.0):
    """The icon's paths. `k` is 1/group scale so stroke and dashes keep the
    width they have on the 512 canvas."""
    p = []
    if paper is not None:
        p.append('{i}<path android:fillColor="{c}" android:pathData="{d}"/>'
                 .format(i=indent, c=paper, d=PAGE))
    p.append('{i}<path android:fillColor="{c}" android:pathData="{d}"/>'
             .format(i=indent, c=fold, d=FOLD))
    if ghost_fill is not None:
        p.append('{i}<path android:fillColor="{c}" android:fillAlpha="{a}" '
                 'android:pathData="{d}"/>'
                 .format(i=indent, c=PAPER, a=ghost_fill, d=GHOST_FILL))
    p.append('{i}<path android:strokeColor="{c}" android:strokeWidth="{w}" '
             'android:strokeLineCap="round" android:strokeLineJoin="round" '
             'android:pathData="{d}"/>'
             .format(i=indent, c=ghost_stroke, w=round(stroke * k, 2),
                     d=dashes(GHOST_LINE, dash[0] * k, dash[1] * k)))
    return "\n".join(p)


def vector(dp, inner, group=False):
    out = HEAD.format(dp=dp)
    if group:
        out += GROUP_OPEN.format(tx=round(SAFE_OFF, 2), ty=round(SAFE_OFF, 2), s=SAFE)
        out += "\n" + inner
        out += GROUP_CLOSE
    else:
        out += "\n" + inner
    out += "\n</vector>\n"
    return out


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)
    print("wrote", os.path.relpath(path, HERE))


# Legacy square icon: background included, full-bleed 108dp canvas.
legacy = ('    <path android:fillColor="{c}" android:pathData="M0,0 H512 V512 H0 Z"/>\n'
          .format(c=BG)) + body()
write(os.path.join(RES, "drawable", "ic_launcher_legacy.xml"),
      vector(108, legacy))

# Adaptive foreground: transparent, art shrunk into the safe zone.
write(os.path.join(RES, "drawable", "ic_launcher_foreground.xml"),
      vector(108, body(k=1 / SAFE), group=True))

# Monochrome (themed icons): one colour, no ghost fill, heavier dashes so the
# shape survives being tinted and drawn small.
mono = body(paper=PAPER, fold=PAPER, ghost_fill=None, ghost_stroke=PAPER,
            dash=(40, 26), stroke=16, k=1 / SAFE)
write(os.path.join(RES, "drawable", "ic_launcher_mono.xml"),
      vector(108, mono, group=True))

ADAPTIVE = """<?xml version="1.0" encoding="utf-8"?>
<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">
    <background android:drawable="@color/icon_bg" />
    <foreground android:drawable="@drawable/ic_launcher_foreground" />
{mono}</adaptive-icon>
"""
write(os.path.join(RES, "mipmap-anydpi-v26", "ic_launcher.xml"),
      ADAPTIVE.format(mono=""))
write(os.path.join(RES, "mipmap-anydpi-v33", "ic_launcher.xml"),
      ADAPTIVE.format(mono='    <monochrome android:drawable="@drawable/ic_launcher_mono" />\n'))

# Pre-26 fallback: the same vector with its background, used as a plain drawable.
write(os.path.join(RES, "mipmap-anydpi", "ic_launcher.xml"),
      vector(108, legacy))
