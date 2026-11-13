#!/usr/bin/env python3
"""What the launcher will actually show, from icon.svg.

The adaptive icon is a transparent background plus the foreground layer
scaled by `fg`; the launcher then cuts it to a circle of `mask` canvas units. This
writes that composition as one SVG so `raster.py` can render it - no Android
in the loop.

    python3 emulate.py            # -> preview-adaptive.svg
"""
import re
import sys

TOK = re.compile(r"[A-Za-z]|-?\d*\.?\d+(?:[eE][-+]?\d+)?")

CX = CY = 256.0


def scale_svg(svg, s, fill_over=None):
    """Scale every coordinate of `svg` about the canvas centre."""
    def num(v):
        return float(v)

    def one_path(m):
        d = m.group(1)
        toks = TOK.findall(d)
        out, i = [], 0
        cmd = None
        while i < len(toks):
            t = toks[i]
            if t.isalpha():
                cmd = t
                out.append(t)
                i += 1
                continue
            n = {"M": 2, "L": 2, "H": 1, "V": 1, "Q": 4, "C": 6, "Z": 0}[cmd.upper()]
            args = toks[i:i + n]
            i += n
            nums = []
            k = 0
            while k < len(args):
                if cmd in "Hh" or cmd in "Vv":
                    base = CX if cmd in "Hh" else CY
                    nums.append("%.3f" % (CX + (num(args[k]) - base) * s))
                    k += 1
                else:
                    nums.append("%.3f" % (CX + (num(args[k]) - CX) * s))
                    nums.append("%.3f" % (CY + (num(args[k + 1]) - CY) * s))
                    k += 2
            out.append(",".join(nums))
        return 'd="%s"' % "".join(out)

    svg = re.sub(r'd="([^"]+)"', lambda m: one_path(m), svg)
    svg = re.sub(r'<circle cx="([\d.]+)" cy="([\d.]+)" r="([\d.]+)"',
                 lambda m: '<circle cx="%.3f" cy="%.3f" r="%.3f"' % (
                     CX + (float(m.group(1)) - CX) * s,
                     CY + (float(m.group(2)) - CY) * s,
                     float(m.group(3)) * s), svg)
    if fill_over:
        svg = svg.replace('fill="#e43717"', 'fill="%s"' % fill_over)
    return svg


def drawable_fg():
    """The scale the generated foreground VectorDrawable actually applies."""
    x = open("../android/res/drawable/ic_launcher_foreground.xml").read()
    m = re.search(r'android:scaleX="([0-9.]+)"', x)
    return float(m.group(1)) if m else 1.0


def main():
    src = open("icon.svg").read()
    mask = float(sys.argv[1]) if len(sys.argv) > 1 else 512.0 / 3.0
    fg = float(sys.argv[2]) if len(sys.argv) > 2 else drawable_fg()
    body = re.search(r"<svg[^>]*>\n(.*)</svg>", src, re.S).group(1)
    fg_svg = scale_svg(body, fg)
    # No background layer: the adaptive icon's background is transparent, so
    # there is nothing behind the gear but the wallpaper.
    out = ('<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512" '
           'viewBox="0 0 512 512">\n'
           '  <!-- emulated adaptive icon: transparent background, fg=%.4f, mask r=%.0f -->\n'
           '%s\n</svg>\n') % (fg, mask, fg_svg)
    open("preview-adaptive.svg", "w").write(out)
    print("wrote preview-adaptive.svg (mask r=%.0f, fg=%.4f -> tips r=%.1f)"
          % (mask, fg, fg * 256.0))


if __name__ == "__main__":
    main()
