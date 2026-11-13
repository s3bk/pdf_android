#!/usr/bin/env python3
"""Draw the pdf-rs viewer icon: the word "PDF" in dark slate on a white bore,
inside a Rust *external* spur gear.

Tulip's gear spec, used verbatim:
    teeth N = 16, module m = 2 mm, pressure angle 25 deg, backlash 0,
    pitch diameter 32 mm, outer rim diameter 41 mm.

That is drawn as the real involute of an internal gear: pitch r 16.00 mm,
base r 14.5009 mm, rim outer r 20.50 mm.  Two consequences of the numbers, both
kept honest in the drawing instead of fudged:

  * the involute does not exist below the base circle, so the tooth tips are
    truncated at r = 14.5009 mm (tip dia 29.0 mm, not 28.0);
  * an internal gear tooth gets THICKER with radius, so the slot between two
    teeth pinches shut at r = 18.63 mm - the slots are therefore stopped at
    r = 17.95 mm, where the gap is still 3.2 deg wide, and the rest is solid
    rim out to 20.50 mm.

Everything is emitted twice from the same numbers:
  * icons/icon.svg, icon-fg.svg, icon-mono.svg, preview-circle.svg, preview.html
  * android/res/drawable/ic_launcher_{legacy,foreground,mono}.xml + adaptive XMLs
The gear is gear.py; the letters are Inter Bold outlines baked into letters.py
(no font at runtime, because a VectorDrawable has no text element).
"""
import math
import os
import re

HERE = os.path.dirname(os.path.abspath(__file__))
RES = os.path.join(HERE, "..", "android", "res")

# --- palette ---------------------------------------------------------------
BG = "#1b2b3a"          # deep slate - the app's own background
RUST = "#e43717"        # Rust rust-orange for the ring gear
PAPER = "#ffffff"
FOLD = "#b9c4d1"

# --- canvas ----------------------------------------------------------------
S = 512.0
CX = CY = 256.0

# --- ring gear: true involute INTERNAL gear --------------------------------
# The geometry lives in gear.py - one generator, two users (this icon and the
# standalone ./gear.py).  Angles there are radians and radii millimetres.
import gear as G  # noqa: E402

TEETH = 16
M_MM = 2.0
ALPHA_DEG = 25.0
BACKLASH_MM = 0.0

_GEAR = G.Gear(TEETH, M_MM, ALPHA_DEG, BACKLASH_MM, kind="external")
_GEAR.verify()                          # raises if the numbers cannot work

R_PITCH = _GEAR.r_pitch                 # 16.000 mm
R_BASE = _GEAR.r_base                   # 14.501 mm
R_TIP = _GEAR.r_tip                     # 18.000 mm: the tooth tips
R_ROOT = _GEAR.r_root                   # = R_BASE: no involute below it

# --- canvas ----------------------------------------------------------------
# An EXTERNAL spur gear: the teeth stand proud of the body, so the silhouette
# is a gear from any distance.  (Tulip: "the red gear is a circle on the
# outside" - the internal ring had its toothing inside the bore and a plain
# round rim outside, which only reads as a gear if you already know.)
#
# FILL = 1.0 puts the tooth tips on the edge of the 512 canvas; the adaptive
# layer scales that down (TIP_FRAC) so the tips land just inside the launcher
# mask, with the outer diameter unchanged.
# Tulip, last word on the shape: "INNER GEAR", and the only defect to fix is
# "the broken outer edge of the red circle".  So the silhouette is a ring gear
# read from outside: ONE clean circle as the outer edge, the 16 teeth pointing
# inward into a white bore, the word in that bore.
#
# Every radius below is millimetres off the gear spec times SCALE, so the
# numbers stay literal: rim outer 20.5 mm, root 18.0 mm, teeth 15.5 mm (1.25 m).
RIM_MM = 20.5                   # outer rim radius = spec's "outer rim dia 41 mm"
MASK_R = 256.0
SCALE = MASK_R / RIM_MM         # canvas units per mm
R_OUT = RIM_MM * SCALE          # 256.0, the one clean circle
R_RIM_IN = 18.63 * SCALE        # 232.6, root circle: the slots pinch shut here
R_TOOTH_IN = 14.5 * SCALE       # 181.1, tooth tips = the spec's tip dia 29 mm
BORE_R = R_RIM_IN               # the white bore is that same circle
MASK_ADAPTIVE = 512.0 / 3.0     # visible circle: 72dp of the 108dp layer
# The launcher shows the central 66.7% of the layer, then masks it.  Tulip:
# "scale the icon up by 2%, centre of scaling in the centre of the gear" - the
# foreground is drawn about the canvas centre, which IS the gear centre, so
# this one factor is that instruction.
ICON_GROW = 1.02
TIP_FRAC = 0.97 * ICON_GROW     # outer circle inside that mask, unclipped
FG = TIP_FRAC * MASK_ADAPTIVE / R_OUT   # adaptive foreground scale


def rim(r_mm):
    return r_mm * SCALE


def _canvas_pts(outline):
    out = []
    for i, (r, th) in enumerate(outline):
        x = CX + rim(r) * math.cos(th)
        y = CY + rim(r) * math.sin(th)
        out.append(("M" if i == 0 else "L") + "{:.2f},{:.2f}".format(x, y))
    return "".join(out) + "Z"


def _arc_pts(r, a0, a1, n=6):
    """Points along an arc - teeth are polygons, so no renderer can misread them."""
    return [(CX + r * math.cos(a0 + (a1 - a0) * i / n),
             CY + r * math.sin(a0 + (a1 - a0) * i / n)) for i in range(n + 1)]


def _poly(pts):
    return "".join(("M" if i == 0 else "L") + "{:.2f},{:.2f}".format(x, y)
                   for i, (x, y) in enumerate(pts)) + "Z"


PITCH_A = 2.0 * math.pi / TEETH
HALF_ROOT = 0.30 * PITCH_A      # tooth half-angle at the root circle
HALF_TIP = 0.21 * PITCH_A       # ... and at the tip: the tooth narrows inward
ROOT_PAD = 4.0                  # the drawn tooth starts inside the rim, so no
                                # white hairline can appear on its root edge


def tooth_pts(k, pad=ROOT_PAD):
    th = k * PITCH_A
    pts = _arc_pts(R_RIM_IN + pad, th - HALF_ROOT, th + HALF_ROOT)
    pts += _arc_pts(R_TOOTH_IN, th + HALF_TIP, th - HALF_TIP)
    return pts


TEETH_PATHS = [_poly(tooth_pts(k)) for k in range(TEETH)]


def circle_path(r):
    """Four quarter arcs, all small-arc, sweep 1: no 180-degree ambiguity.

    Every endpoint must be one of the four axis points - (CX-r, CY),
    (CX, CY-r), (CX+r, CY), (CX, CY+r) - or the four arcs land elsewhere and
    the shape renders as several overlapping circles instead of one.
    """
    return ("M{0:.2f},{1:.2f} A{2:.2f},{2:.2f} 0 0 1 {3:.2f},{4:.2f} "
            "A{2:.2f},{2:.2f} 0 0 1 {5:.2f},{1:.2f} "
            "A{2:.2f},{2:.2f} 0 0 1 {3:.2f},{6:.2f} "
            "A{2:.2f},{2:.2f} 0 0 1 {0:.2f},{1:.2f} Z").format(
                CX - r, CY, r, CX, CY - r, CX + r, CY + r)


BORE = circle_path(BORE_R)


# --- the word "PDF" ---------------------------------------------------------
# Real Inter Bold outlines - the app's own font - extracted once from
# assets/Inter.ttf by extract_letters.py into letters.py, because a
# VectorDrawable has no text and the icon must not depend on a font.
from letters import PDF_PATH as WORD, CAP_H as H_CAP, INK_L, INK_R  # noqa: E402

WORD_W = INK_R - INK_L


def scale_path(d, s):
    """Scale every absolute coordinate about (CX, CY); H/V take one number.

    letters.py writes plain absolute M/L/H/V/Q paths, so this is exact - no
    transform attribute, which also means the VectorDrawable needs none.
    """
    out, i, toks = [], 0, _PATH_TOK.findall(d)
    while i < len(toks):
        c = toks[i]
        i += 1
        if c in "Zz":
            out.append(c)
            continue
        if c in "Aa":
            raise ValueError("arcs are not used in the word")
        elif c in "HhVv":
            n = 1
        elif c in "MmTt":
            n = 2
        elif c in "QqSs":
            n = 4
        elif c in "Cc":
            n = 6
        else:
            raise ValueError("unsupported command " + c)
        args = [float(v) for v in toks[i:i + n]]
        i += n
        out.append(c)
        nums = []
        if n == 1:
            if c in "Hh":
                nums.append(_fmt(CX + (args[0] - CX) * s))
            else:
                nums.append(_fmt(CY + (args[0] - CY) * s))
        else:
            for k in range(0, n, 2):
                nums.append(_fmt(CX + (args[k] - CX) * s))
                nums.append(_fmt(CY + (args[k + 1] - CY) * s))
        # numbers MUST be separated: "114.48" + "312.38" would read as one
        # number 114.48312 and a stray .38 - the bug Tulip saw.
        out.append(",".join(nums))
    return "".join(out)


def _fmt(v):
    return ("{:.2f}".format(v).rstrip("0").rstrip(".") or "0")


_PATH_TOK = re.compile(r"[A-Za-z]|-?\d*\.?\d+(?:[eE][-+]?\d+)?")

# Fit the word's bounding box inside the bore, a little smaller than the bore
# so it never touches the rust body.
WORD_FIT_R = R_TOOTH_IN - 18.0
_a = WORD_FIT_R / math.hypot(1.0, (H_CAP / WORD_W))
WORD_SCALE = 2.0 * _a / WORD_W
WORD_FIT = scale_path(WORD, WORD_SCALE)


def _num(v):
    """A number as SVG/XML wants it: no trailing zeros, no exponent."""
    return "{:.2f}".format(v).rstrip("0").rstrip(".")


# --- SVG -------------------------------------------------------------------
def svg(shapes, size=512, bg=None):
    body = []
    if bg:
        body.append('  <rect width="{0}" height="{0}" fill="{1}"/>'.format(size, bg))
    for sh in shapes:
        if "circle" in sh:
            body.append('  <circle cx="{0:.2f}" cy="{1:.2f}" r="{2}" fill="{3}"/>'.format(
                CX, CY, _num(sh["circle"]), sh.get("fill", "none")))
            continue
        attrs = 'd="{}"'.format(sh["d"])
        for key in ("fill", "stroke", "stroke-width", "fill-rule", "fill-opacity",
                    "stroke-linecap", "stroke-dasharray"):
            if key in sh:
                v = sh[key]
                if isinstance(v, (int, float)):
                    v = "{:.2f}".format(v).rstrip("0").rstrip(".")
                attrs += ' {}="{}"'.format(key, v)
        body.append("  <path " + attrs + "/>")
    return ('<svg xmlns="http://www.w3.org/2000/svg" width="{0}" height="{0}" '
            'viewBox="0 0 {0} {0}">\n'.format(size) + "\n".join(body) + "\n</svg>\n")


def shapes_full():
    # Stack, never a hole sub-path: one rust circle (the unbroken outer edge),
    # the white bore painted on top of it, the 16 teeth painted on top of that,
    # the word last.  Every edge antialiases against what it really sits on, so
    # nothing is cut out and nothing shows a seam.
    return ([dict(circle=R_OUT, fill=RUST),
             dict(circle=BORE_R, fill=PAPER)]
            + [dict(d=d, fill=RUST) for d in TEETH_PATHS]
            + [dict(d=WORD_FIT, fill=BG, **{"fill-rule": "nonzero"})])


def shapes_glyph():
    """Artwork on transparent, for the adaptive foreground."""
    return shapes_full()


def shapes_mono():
    """One colour, no overlapping sub-paths: outer circle, bore circle, the 16
    teeth, the word - even-odd.  Rim = inside 1 loop, tooth = 3, bore gap = 2,
    letter = 3: filled, filled, empty, filled."""
    parts = [circle_path(R_OUT), circle_path(BORE_R)]
    parts += [_poly(tooth_pts(k, pad=0.0)) for k in range(TEETH)]
    parts.append(WORD_FIT)
    return [dict(d=" ".join(parts), fill=PAPER, **{"fill-rule": "evenodd"})]


def write(path, text):
    with open(path, "w") as f:
        f.write(text)
    print("wrote", os.path.relpath(path, HERE), len(text), "B")


# --- Android VectorDrawables ----------------------------------------------
def circle_data(r):
    """A <circle> as pathData, for the one format that has no circle element.

    Two half circles, large-arc flag set: the points are the two axis points, so
    the arcs cannot land anywhere but on the circle.
    """
    return ("M{0:.2f},{1:.2f} A{2:.2f},{2:.2f} 0 1 1 {3:.2f},{1:.2f} "
            "A{2:.2f},{2:.2f} 0 1 1 {0:.2f},{1:.2f} Z").format(
                CX - r, CY, r, CX + r)


def vd_path(sh):
    if "circle" in sh:
        sh = dict(sh)
        sh["d"] = circle_data(sh.pop("circle"))
        sh.setdefault("fill", "none")
    attrs = 'android:pathData="{}"'.format(sh["d"])
    if sh.get("fill"):
        attrs += ' android:fillColor="{}"'.format(sh["fill"])
    if sh.get("fill-rule") == "evenodd":
        attrs += ' android:fillType="evenOdd"'
    if sh.get("stroke"):
        attrs += ' android:strokeColor="{}"'.format(sh["stroke"])
        attrs += ' android:strokeWidth="{:.2f}"'.format(sh.get("stroke-width", 1))
        attrs += (' android:strokeLineCap="round" '
                  'android:strokeLineJoin="round"')
    return '        <path ' + attrs + '/>'


def vector(dp, shapes, scale=1.0):
    head = ('<?xml version="1.0" encoding="utf-8"?>\n'
            '<!-- Generated by icons/draw_icon.py from the same geometry as\n'
            '     icons/icon.svg - edit the script, then rerun it. -->\n'
            '<vector xmlns:android="http://schemas.android.com/apk/res/android"\n'
            '    android:width="{dp}dp"\n'
            '    android:height="{dp}dp"\n'
            '    android:viewportWidth="512"\n'
            '    android:viewportHeight="512">\n').format(dp=dp)
    out = head
    if scale != 1.0:
        off = 256.0 * (1 - scale)
        out += ('    <group android:translateX="{:.2f}" '
                'android:translateY="{:.2f}" android:scaleX="{s}" '
                'android:scaleY="{s}">\n').format(off, off, s=scale)
    out += "\n".join(vd_path(sh) for sh in shapes) + "\n"
    if scale != 1.0:
        out += "    </group>\n"
    out += "</vector>\n"
    return out


def svg_masked(shapes, bg="#f2f4f7", r=248):
    """The same art as a launcher would mask it, on a light field."""
    parts = []
    for sh in shapes:
        if "circle" in sh:
            parts.append('    <circle cx="{0:.2f}" cy="{1:.2f}" r="{2}" fill="{3}"/>'.format(
                CX, CY, _num(sh["circle"]), sh.get("fill", "none")))
            continue
        fr = ' fill-rule="evenodd"' if sh.get("fill-rule") == "evenodd" else ""
        parts.append('    <path d="{0}" fill="{1}"{2}/>'.format(
            sh["d"], sh.get("fill", "none"), fr))
    return ('<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512" '
            'viewBox="0 0 512 512">\n'
            '  <defs><clipPath id="mask"><circle cx="256" cy="256" r="{r}"/></clipPath></defs>\n'
            '  <rect width="512" height="512" fill="{bg}"/>\n'
            '  <g clip-path="url(#mask)">\n' + "\n".join(parts) + '\n  </g>\n</svg>\n').format(r=r, bg=bg)


PREVIEW_HTML = """<!doctype html>
<meta charset="utf-8">
<title>pdf-rs viewer icon</title>
<style>
  body {{ margin: 0; background: #f2f4f7; font: 12px/1.4 system-ui, sans-serif;
         color: #3b444e; display: flex; gap: 34px; padding: 28px; align-items: flex-start; }}
  figure {{ margin: 0; text-align: center; }}
  figcaption {{ margin-top: 8px; }}
  img {{ display: block; border-radius: 22.4%; }}
  .plain img {{ border-radius: 0; }}
</style>
<figure class="plain"><img src="icon.svg" width="256" height="256"><figcaption>256 px, square</figcaption></figure>
<figure><img src="icon.svg" width="144" height="144"><figcaption>144 px</figcaption></figure>
<figure><img src="icon.svg" width="72" height="72"><figcaption>72 px</figcaption></figure>
<figure><img src="icon.svg" width="48" height="48"><figcaption>48 px</figcaption></figure>
<figure><img src="preview-circle.svg" width="256" height="256"><figcaption>launcher mask</figcaption></figure>
"""


def main():
    print("gear: pitch r {:.3f} mm, tip r {:.3f} mm, root {:.3f} mm"
          .format(R_PITCH, R_TIP, R_ROOT))
    print("      slot half-angle at tip {:.2f} deg, at root {:.2f} deg"
          .format(math.degrees(_GEAR.half_gap(R_TIP)),
                  math.degrees(_GEAR.half_gap(R_ROOT))))
    print("word: {:.1f} x {:.1f} canvas units, scale {:.3f} of the font's em box"
          .format(WORD_W * WORD_SCALE, H_CAP * WORD_SCALE, WORD_SCALE))
    print("      outer r {:.1f}, teeth root r {:.1f}, tips r {:.1f}, "
          "word circle r {:.1f}, fg {:.6f}"
          .format(R_OUT, R_RIM_IN, R_TOOTH_IN, WORD_FIT_R, FG))

    # source of truth
    write(os.path.join(HERE, "icon.svg"), svg(shapes_full(), bg=None))
    write(os.path.join(HERE, "icon-fg.svg"), svg(shapes_glyph()))
    write(os.path.join(HERE, "icon-mono.svg"), svg(shapes_mono(), bg=None))
    write(os.path.join(HERE, "preview-circle.svg"), svg_masked(shapes_full()))
    write(os.path.join(HERE, "preview.html"), PREVIEW_HTML)

    # Android.
    # The launcher scales the 108dp layer up so that only its central 72dp is
    # visible (AdaptiveIconDrawable's 25% inset), and then masks that circle:
    # radius 512/3 = 170.7 in this 512 viewport.  Anything drawn outside that
    # is cropped, which is how the gear kept turning into a plain disc - the
    # tips were cut square onto the mask circle.  So the tooth tips go to
    # TIP_FRAC of 170.7: the outer diameter is the icon circle and the teeth
    # that make the silhouette are inside it, unclipped.
    # The background layer is the icon's own rust.  With a transparent layer
    # the 5 units between the outer circle (165.6) and the mask (170.7) showed
    # through as a dark band, which reads as a broken edge on the device.
    # Rust behind a ring gear is harmless: the teeth live inside the white
    # bore, so nothing of the background is ever visible except that band -
    # and filling it is the point.
    fg = FG
    leg = 0.92
    write(os.path.join(RES, "drawable", "ic_launcher_foreground.xml"),
          vector(108, shapes_glyph(), scale=fg))
    write(os.path.join(RES, "drawable", "ic_launcher_legacy.xml"),
          vector(108, shapes_full(), scale=leg))
    write(os.path.join(RES, "drawable", "ic_launcher_mono.xml"),
          vector(108, shapes_mono(), scale=fg))

    adaptive = ('<?xml version="1.0" encoding="utf-8"?>\n'
                '<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">\n'
                '    <background android:drawable="@color/icon_rim" />\n'
                '    <foreground android:drawable="@drawable/ic_launcher_foreground" />\n'
                '{mono}</adaptive-icon>\n')
    write(os.path.join(RES, "mipmap-anydpi-v26", "ic_launcher.xml"),
          adaptive.format(mono=""))
    write(os.path.join(RES, "mipmap-anydpi-v33", "ic_launcher.xml"),
          adaptive.format(mono='    <monochrome android:drawable="@drawable/ic_launcher_mono" />\n'))
    write(os.path.join(RES, "mipmap-anydpi", "ic_launcher.xml"),
          vector(108, shapes_full(), scale=leg))


if __name__ == "__main__":
    main()
