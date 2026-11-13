#!/usr/bin/env python3
"""The app icon is Tulip's logo.svg (icons/logo.svg) - black gear, white bore,
black "PDF" - on a WHITE background.

Tulip: "use this as app icon. white background".

So this script is the source of truth for the launcher assets now; draw_icon.py
(and make_icons.py before it) generated the old rust gear.  Nothing is redrawn
here: the paths of logo.svg are taken verbatim, only scaled and translated, so
what lands in the APK is exactly what he sent.

logo.svg is an Inkscape file: viewBox 144 x 144, artwork inside one group that
scales the inner 0..108 space by 4/3.  In that inner space the gear is centred
on (54,54) with outer radius 53.32, the bore is a circle of radius 38.34, and
the three letters sit under their own matrix(0.75,0,0,0.75,-2.265,0.229).

Outputs
  icons/icon.svg                512 square, white ground (preview / upload)
  icons/icon-fg.svg             adaptive foreground, transparent
  android/res/drawable/ic_launcher_{legacy,foreground,mono}.xml
  android/res/mipmap-anydpi*/ic_launcher.xml
  android/res/values/colors.xml icon_rim = the icon's white
"""
import os
import re

HERE = os.path.dirname(os.path.abspath(__file__))
RES = os.path.join(HERE, "..", "android", "res")
LOGO = os.path.join(HERE, "logo.svg")

BLACK = "#000000"
WHITE = "#ffffff"

# Canvas of the emitted art, and the art's own geometry in logo units.
S = 512.0
C = 256.0
LOGO_CX = LOGO_CY = 54.0
R_LOGO = 53.32          # gear outer radius: the path's top point is y = 0.68
BORE_R = 38.340175      # path16 is a circle of this radius about (54,54)
# Letter paths live in their own 0.75x space: p' = 0.75 * p + (e, f).
LETTER_K = 0.75
LETTER_E = -2.2651387
LETTER_F = 0.22881087

# How much of the launcher to use.  512/3 = 170.67 is the visible circle of the
# 108dp adaptive layer (the launcher crops the rest, then masks); 256 is the
# legacy square.  A clean round gear may go closer to the edge than a toothed
# silhouette, but not onto it: 0.96 leaves a white hairline ring.
FILL_ADAPTIVE = 0.96 * (S / 3.0)
FILL_LEGACY = 0.90 * (S / 2.0)

TOK = re.compile(r"[A-Za-z]|-?\d*\.?\d+(?:[eE][-+]?\d+)?")
# numbers per command; M is special (a second pair is a lineto)
NARG = dict(M=2, L=2, H=1, V=1, C=6, S=4, Q=4, T=2, Z=0)


def _num(v):
    return "{:.2f}".format(v).rstrip("0").rstrip(".") or "0"


def transform(d, s, tx, ty):
    """Linear map p -> s*p + (tx,ty), applied to path data.

    Absolute commands are rewritten with the full map.  Relative commands stay
    relative - their numbers are deltas, so they only get multiplied by s -
    which also keeps them correct against the moved current point.

    The one exception is the FIRST command: a path starts at (0,0), so an
    opening relative "m" is not relative to anything and is emitted as an
    absolute M with the translation folded in (without that the whole subpath
    landed `tx` too far left).  Its implicit continuation pairs are then
    linetos relative to that moved point, i.e. plain scaled deltas.

    Every number set gets its own command letter: no renderer has to guess at
    implicit repeats.
    """
    out = []
    toks = TOK.findall(d)
    i = 0
    cmd = None
    first = True
    while i < len(toks):
        t = toks[i]
        if t.isalpha():
            cmd = t
            i += 1
            if cmd in "Zz":
                out.append("Z")
                continue
        upper = cmd.upper()
        rel = cmd.islower()
        n = NARG[upper]
        args = [float(v) for v in toks[i:i + n]]
        i += n
        opening = first and upper == "M"
        letter = "M" if opening else (cmd.lower() if rel else upper)
        nums = []
        if upper in "HV":
            v = args[0]
            shift = 0.0 if (rel and not opening) else (tx if upper == "H" else ty)
            nums.append(_num(v * s + shift))
        else:
            for k in range(0, n, 2):
                x, y = args[k], args[k + 1]
                if rel and not opening:
                    nums.append(_num(x * s))
                    nums.append(_num(y * s))
                else:
                    nums.append(_num(x * s + tx))
                    nums.append(_num(y * s + ty))
        out.append(letter + ",".join(nums))
        # After a moveto the following numbers are linetos: relative for "m"
        # (scaled deltas from the point we just moved to), absolute for "M".
        if upper == "M":
            cmd = "l" if rel else "L"
        first = False
    return "".join(out)


def circle_data(cx, cy, r):
    """A circle as two half arcs (VectorDrawables have no <circle>)."""
    return ("M{0:.2f},{1:.2f} A{2:.2f},{2:.2f} 0 1 1 {3:.2f},{1:.2f} "
            "A{2:.2f},{2:.2f} 0 1 1 {0:.2f},{1:.2f} Z").format(cx - r, cy, r, cx + r)


# --- read the logo ---------------------------------------------------------
_src = open(LOGO).read()


def _d(path_id):
    m = re.search(r'\sd="([^"]+)"[^>]*\sid="' + path_id + '"', _src, re.S)
    if not m:
        raise SystemExit("path " + path_id + " not found in " + LOGO)
    return m.group(1)


GEAR = _d("path14")                     # the gear silhouette, one closed loop
BORE = _d("path16")                     # its white bore
LETTERS = [_d("path22"), _d("path24"), _d("path26")]


def art(s, tx, ty):
    """(pathData, colour) in the 512 canvas: gear, bore, the word."""
    sl = LETTER_K * s
    tlx = s * LETTER_E + tx
    tly = s * LETTER_F + ty
    out = [(transform(GEAR, s, tx, ty), BLACK),
           (transform(BORE, s, tx, ty), WHITE)]
    out += [(transform(d, sl, tlx, tly), BLACK) for d in LETTERS]
    return out


def mono(s, tx, ty):
    """One colour, one path: gear filled, bore a hole, letters filled again.
    even-odd: loop 1 = in, 2 = out, 3.. = in."""
    sl = LETTER_K * s
    tlx = s * LETTER_E + tx
    tly = s * LETTER_F + ty
    parts = [transform(GEAR, s, tx, ty), transform(BORE, s, tx, ty)]
    parts += [transform(d, sl, tlx, tly) for d in LETTERS]
    return " ".join(parts)


def scaled(target_r):
    s = target_r / R_LOGO
    return s, C - s * LOGO_CX, C - s * LOGO_CY


# --- SVG -------------------------------------------------------------------
def svg(shapes, bg=None):
    body = []
    if bg:
        body.append('  <rect width="{0:.0f}" height="{0:.0f}" fill="{1}"/>'.format(S, bg))
    for d, colour in shapes:
        body.append('  <path d="{}" fill="{}"/>'.format(d, colour))
    return ('<svg xmlns="http://www.w3.org/2000/svg" width="{0:.0f}" height="{0:.0f}" '
            'viewBox="0 0 {0:.0f} {0:.0f}">\n'.format(S) + "\n".join(body) + "\n</svg>\n")


# --- Android ---------------------------------------------------------------
def vector(dp, shapes, scale=1.0):
    head = ('<?xml version="1.0" encoding="utf-8"?>\n'
            '<!-- Generated by icons/logo_icon.py from Tulip\'s icons/logo.svg.\n'
            '     Do not edit: change logo.svg and rerun the script. -->\n'
            '<vector xmlns:android="http://schemas.android.com/apk/res/android"\n'
            '    android:width="{dp}dp"\n'
            '    android:height="{dp}dp"\n'
            '    android:viewportWidth="512"\n'
            '    android:viewportHeight="512">\n').format(dp=dp)
    out = head
    for d, colour in shapes:
        out += ('    <path android:fillColor="{}" android:pathData="{}"/>\n'
                .format(colour, d))
    out += "</vector>\n"
    return out


def vector_mono(dp, d):
    return ('<?xml version="1.0" encoding="utf-8"?>\n'
            '<!-- Generated by icons/logo_icon.py from Tulip\'s icons/logo.svg. -->\n'
            '<vector xmlns:android="http://schemas.android.com/apk/res/android"\n'
            '    android:width="{dp}dp"\n'
            '    android:height="{dp}dp"\n'
            '    android:viewportWidth="512"\n'
            '    android:viewportHeight="512">\n'
            '    <path android:fillColor="#ffffff" android:fillType="evenOdd"\n'
            '        android:pathData="{d}"/>\n'
            '</vector>\n').format(dp=dp, d=d)


ADAPTIVE = """<?xml version="1.0" encoding="utf-8"?>
<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">
    <background android:drawable="@color/icon_rim" />
    <foreground android:drawable="@drawable/ic_launcher_foreground" />
{mono}</adaptive-icon>
"""

COLORS = """<?xml version="1.0" encoding="utf-8"?>
<resources>
    <!-- The icon is Tulip's logo: black gear, white bore, black "PDF", on
         white.  The adaptive background layer is therefore white, and the same
         white is the window background, so a cold start flashes the icon's own
         colour.  (Was the old gear's rust.) -->
    <color name="icon_rim">#ffffff</color>
    <!-- Kept for older launchers and the Settings page. -->
    <color name="icon_bg">#ffffff</color>
</resources>
"""


def write(path, text):
    with open(path, "w") as f:
        f.write(text)
    print("wrote", os.path.relpath(path, HERE), len(text), "B")


def main():
    # previews / uploads
    write(os.path.join(HERE, "icon.svg"), svg(art(*scaled(FILL_LEGACY)), bg=WHITE))
    write(os.path.join(HERE, "icon-fg.svg"), svg(art(*scaled(FILL_ADAPTIVE))))
    write(os.path.join(HERE, "icon-mono.svg"),
          svg([(mono(*scaled(FILL_LEGACY)), BLACK)]))

    # Android
    write(os.path.join(RES, "drawable", "ic_launcher_foreground.xml"),
          vector(108, art(*scaled(FILL_ADAPTIVE))))
    write(os.path.join(RES, "drawable", "ic_launcher_legacy.xml"),
          vector(108, art(*scaled(FILL_LEGACY))))
    write(os.path.join(RES, "drawable", "ic_launcher_mono.xml"),
          vector_mono(108, mono(*scaled(FILL_ADAPTIVE))))
    write(os.path.join(RES, "mipmap-anydpi-v26", "ic_launcher.xml"),
          ADAPTIVE.format(mono=""))
    write(os.path.join(RES, "mipmap-anydpi-v33", "ic_launcher.xml"),
          ADAPTIVE.format(mono='    <monochrome android:drawable="@drawable/ic_launcher_mono" />\n'))
    write(os.path.join(RES, "mipmap-anydpi", "ic_launcher.xml"),
          vector(108, art(*scaled(FILL_LEGACY))))
    write(os.path.join(RES, "values", "colors.xml"), COLORS)

    s, tx, ty = scaled(FILL_ADAPTIVE)
    print("adaptive: gear r {:.2f} of the 170.67 mask, scale {:.4f}"
          .format(R_LOGO * s, s))


if __name__ == "__main__":
    main()
