#!/usr/bin/env python3
"""The app icon is Tulip's icons/spur36-tulip.svg: a 36-tooth spur gear with
"PDF" cut out of its bore - BLACK ring on a WHITE ground.

Tulip: "take icon-spur36.svg, make the red ring black and set background to
white" and then, pointing at the file itself, "this".

Nothing is redrawn: both paths of his SVG are taken verbatim and only scaled
and translated into the 512 canvas the launcher assets use.  The only colour
change is his #333333 gear -> #000000.

His canvas is already 512 and the gear is centred on (256,256) with its tooth
tips at radius ~237.9, so a single uniform map about that centre does the job:

    p' = s * (p - 256) + 256        s = target_radius / 237.9

Outputs
  icons/icon.svg                512 square, white ground (preview / upload)
  icons/icon-fg.svg             adaptive foreground, transparent
  icons/icon-mono.svg           themed layer, one colour
  android/res/drawable/ic_launcher_{legacy,foreground,mono}.xml
  android/res/mipmap-anydpi*/ic_launcher.xml
  android/res/values/colors.xml icon_rim = white
"""
import os
import re

HERE = os.path.dirname(os.path.abspath(__file__))
RES = os.path.join(HERE, "..", "android", "res")
SRC = os.path.join(HERE, "spur36-tulip.svg")

BLACK = "#000000"
WHITE = "#ffffff"

S = 512.0
C = 256.0
R_SRC = 237.9           # gear tooth-tip radius in his file (bbox 18.09..493.91)

# 512/3 = 170.67 is the launcher's visible circle of the 108dp adaptive layer;
# 256 is the legacy square.  0.96 keeps a white hairline inside the mask.
FILL_ADAPTIVE = 0.96 * (S / 3.0)
FILL_LEGACY = 0.90 * (S / 2.0)

TOK = re.compile(r"[A-Za-z]|-?\d*\.?\d+(?:[eE][-+]?\d+)?")
NARG = dict(M=2, L=2, H=1, V=1, C=6, S=4, Q=4, T=2, Z=0, A=7)


def _num(v):
    return "{:.2f}".format(v).rstrip("0").rstrip(".") or "0"


def transform(d, s, tx, ty):
    """p -> s*p + (tx,ty) applied to path data, arcs included.

    Relative commands stay relative: their numbers are deltas, so scaling is
    all they get.  An arc's radii scale, its rotation and its two flags do not,
    and its endpoint is a point like any other.
    """
    out, toks, i, cmd, first = [], TOK.findall(d), 0, None, True
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
        elif upper == "A":
            nums += [_num(args[0] * s), _num(args[1] * s),
                     _num(args[2]), str(int(args[3])), str(int(args[4]))]
            if rel and not opening:
                nums += [_num(args[5] * s), _num(args[6] * s)]
            else:
                nums += [_num(args[5] * s + tx), _num(args[6] * s + ty)]
        else:
            for k in range(0, n, 2):
                x, y = args[k], args[k + 1]
                if rel and not opening:
                    nums += [_num(x * s), _num(y * s)]
                else:
                    nums += [_num(x * s + tx), _num(y * s + ty)]
        out.append(letter + ",".join(nums))
        if upper == "M":
            cmd = "l" if rel else "L"
        first = False
    return "".join(out)


def _read():
    """[(pathData, fill)] of his SVG, in document order."""
    src = open(SRC).read()
    out = []
    for m in re.finditer(r"<path\b(.*?)/>", src, re.S):
        body = m.group(1)
        d = re.search(r'\sd="([^"]+)"', body, re.S)
        fill = re.search(r"fill:(#[0-9a-fA-F]{6})", body) or \
            re.search(r'fill="(#[0-9a-fA-F]{6})"', body)
        if not d:
            continue
        colour = (fill.group(1).lower() if fill else BLACK)
        # his dark grey gear becomes black; everything else is black already
        out.append((d.group(1), BLACK if colour in ("#333333", "#000000") else colour))
    if not out:
        raise SystemExit("no paths in " + SRC)
    return out


PATHS = _read()


def scaled(target_r):
    s = target_r / R_SRC
    return s, C - s * C, C - s * C


def art(target_r):
    s, tx, ty = scaled(target_r)
    return [(transform(d, s, tx, ty), colour) for d, colour in PATHS]


def mono(target_r):
    """One path, one colour: the two paths concatenated (each keeps its own
    winding, which is what makes the bore and the letter counters holes)."""
    return " ".join(d for d, _ in art(target_r))


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
def vector(dp, shapes):
    out = ('<?xml version="1.0" encoding="utf-8"?>\n'
           '<!-- Generated by icons/spur_icon.py from Tulip\'s icons/spur36-tulip.svg.\n'
           '     Do not edit: change the SVG and rerun the script. -->\n'
           '<vector xmlns:android="http://schemas.android.com/apk/res/android"\n'
           '    android:width="{dp}dp"\n'
           '    android:height="{dp}dp"\n'
           '    android:viewportWidth="512"\n'
           '    android:viewportHeight="512">\n').format(dp=dp)
    for d, colour in shapes:
        out += ('    <path android:fillColor="{}" android:pathData="{}"/>\n'
                .format(colour, d))
    return out + "</vector>\n"


def vector_mono(dp, d):
    return ('<?xml version="1.0" encoding="utf-8"?>\n'
            '<!-- Generated by icons/spur_icon.py. -->\n'
            '<vector xmlns:android="http://schemas.android.com/apk/res/android"\n'
            '    android:width="{dp}dp"\n'
            '    android:height="{dp}dp"\n'
            '    android:viewportWidth="512"\n'
            '    android:viewportHeight="512">\n'
            '    <path android:fillColor="#ffffff"\n'
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
    <!-- Black spur gear, white bore, black "PDF", on white.  The adaptive
         background layer is white, and the same white is the window
         background, so a cold start flashes the icon's own colour. -->
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
    write(os.path.join(HERE, "icon.svg"), svg(art(FILL_LEGACY), bg=WHITE))
    write(os.path.join(HERE, "icon-fg.svg"), svg(art(FILL_ADAPTIVE)))
    write(os.path.join(HERE, "icon-mono.svg"),
          svg([(mono(FILL_LEGACY), BLACK)]))

    write(os.path.join(RES, "drawable", "ic_launcher_foreground.xml"),
          vector(108, art(FILL_ADAPTIVE)))
    write(os.path.join(RES, "drawable", "ic_launcher_legacy.xml"),
          vector(108, art(FILL_LEGACY)))
    write(os.path.join(RES, "drawable", "ic_launcher_mono.xml"),
          vector_mono(108, mono(FILL_ADAPTIVE)))
    write(os.path.join(RES, "mipmap-anydpi-v26", "ic_launcher.xml"),
          ADAPTIVE.format(mono=""))
    write(os.path.join(RES, "mipmap-anydpi-v33", "ic_launcher.xml"),
          ADAPTIVE.format(mono='    <monochrome android:drawable="@drawable/ic_launcher_mono" />\n'))
    write(os.path.join(RES, "mipmap-anydpi", "ic_launcher.xml"),
          vector(108, art(FILL_LEGACY)))
    write(os.path.join(RES, "values", "colors.xml"), COLORS)

    s, tx, ty = scaled(FILL_ADAPTIVE)
    print("adaptive: gear tip radius {:.2f} of the 170.67 mask, scale {:.4f}"
          .format(R_SRC * s, s))


if __name__ == "__main__":
    main()
