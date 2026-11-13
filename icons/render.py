#!/usr/bin/env python3
"""Rasterise the icon from the SAME shape list that goes into the app.

`draw_icon.py` emits SVGs and VectorDrawables; this reads the identical shapes
from it and fills them with a scanline renderer, so what is checked here is
what Android is given - not a second, differently-buggy picture.

  python3 render.py OUT.png [PX] [MASK] [--ascii]

MASK is the radius of the launcher's circular mask in 512 canvas units (0 or
omitted = square, unmasked).  Colours: rust ring, white bore, slate word.
"""
import math
import os
import struct
import sys
import zlib

import draw_icon as D

RUST = (0xE4, 0x37, 0x17)
PAPER = (0xFF, 0xFF, 0xFF)
SLATE = (0x1B, 0x2B, 0x3A)

TOK = __import__("re").compile(r"[A-Za-z]|-?\d*\.?\d+(?:[eE][-+]?\d+)?")


def flatten(d, steps=10):
    """Path -> list of polygons (absolute M/L/H/V/Z/Q; Q is flattened)."""
    toks = TOK.findall(d)
    i, polys, cur, start, pos = 0, [], [], None, (0.0, 0.0)
    while i < len(toks):
        c = toks[i]
        i += 1
        if c in "Mm":
            x, y = float(toks[i]), float(toks[i + 1])
            i += 2
            if cur:
                polys.append(cur)
            cur = [(x, y)]
            start = (x, y)
            pos = (x, y)
        elif c == "L":
            x, y = float(toks[i]), float(toks[i + 1])
            i += 2
            cur.append((x, y))
            pos = (x, y)
        elif c == "H":
            x = float(toks[i]); i += 1
            pos = (x, pos[1]); cur.append(pos)
        elif c == "V":
            y = float(toks[i]); i += 1
            pos = (pos[0], y); cur.append(pos)
        elif c == "Q":
            cx, cy, x, y = (float(v) for v in toks[i:i + 4])
            i += 4
            x0, y0 = pos
            for k in range(1, steps + 1):
                t = k / steps
                m = 1 - t
                cur.append((m * m * x0 + 2 * m * t * cx + t * t * x,
                            m * m * y0 + 2 * m * t * cy + t * t * y))
            pos = (x, y)
        elif c in "Zz":
            if cur:
                polys.append(cur)
            cur = []
            pos = start
        else:
            raise ValueError("unsupported path command %r" % c)
    if cur:
        polys.append(cur)
    return polys


def circles_to_polys(r, n=180):
    return [(D.CX + r * math.cos(2 * math.pi * i / n),
             D.CY + r * math.sin(2 * math.pi * i / n)) for i in range(n)]


SCALE_ART = 1.0     # the adaptive layer's group scale (D.FG), for on-device size


def _xform(polys):
    if SCALE_ART == 1.0:
        return polys
    return [[(D.CX + (x - D.CX) * SCALE_ART, D.CY + (y - D.CY) * SCALE_ART)
             for x, y in poly] for poly in polys]


def shape_polys(sh):
    """Every shape as (polygons, colour, winding rule)."""
    rule = sh.get("fill-rule", "nonzero")
    if "circle" in sh:
        return _xform([circles_to_polys(sh["circle"])]), sh["fill"], rule
    return _xform(flatten(sh["d"])), sh["fill"], rule


def inside(polys, x, y, rule="nonzero"):
    w = 0
    for poly in polys:
        n = len(poly)
        for i in range(n):
            x0, y0 = poly[i]
            x1, y1 = poly[(i + 1) % n]
            if (y0 <= y < y1) or (y1 <= y < y0):
                t = (y - y0) / (y1 - y0)
                if x0 + t * (x1 - x0) > x:
                    w += 1 if y1 > y0 else -1
    if rule == "evenodd":
        return w % 2 == 1
    return w != 0


def colour_of(c):
    if isinstance(c, str):
        c = c.lstrip("#")
        return (int(c[0:2], 16), int(c[2:4], 16), int(c[4:6], 16))
    return c


def render(px=512, mask=0.0, ss=3, shapes=None):
    shapes = shapes or D.shapes_full()
    prepped = [(p, colour_of(col), rule) for p, col, rule in
               (shape_polys(sh) for sh in shapes)]
    # When a mask is given, render exactly the launcher's disc: the sample
    # window is the mask's bounding box, so px is the size on screen.
    half = mask if mask else 256.0
    k = 2.0 * half / px                # canvas units per output pixel
    img = []
    for py in range(px):
        row = []
        for pxi in range(px):
            r = g = b = 0
            for sy in range(ss):
                for sx in range(ss):
                    x = D.CX - half + (pxi + (sx + 0.5) / ss) * k
                    y = D.CY - half + (py + (sy + 0.5) / ss) * k
                    col = None
                    if mask and math.hypot(x - D.CX, y - D.CY) > mask:
                        row_alpha = 0          # outside the launcher's disc
                    else:
                        row_alpha = 255
                        for polys, c, rule in prepped:
                            if inside(polys, x, y, rule):
                                col = c
                    if row_alpha:
                        c = col or (0xF2, 0xF4, 0xF7)
                        r += c[0]; g += c[1]; b += c[2]
            n = ss * ss
            if r or g or b:
                row.append((r // n, g // n, b // n, 255))
            else:
                row.append((0, 0, 0, 0))
        img.append(row)
    return img


def write_png(path, img):
    h = len(img); w = len(img[0])
    raw = b"".join(b"\x00" + b"".join(bytes(px) for px in row) for row in img)

    def chunk(t, d):
        return (struct.pack(">I", len(d)) + t + d
                + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF))

    png = (b"\x89PNG\r\n\x1a\n"
           + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))
    open(path, "wb").write(png)


ASCII = "#@%*+=-:. "


def ascii_art(img, step=2):
    h = len(img); w = len(img[0])
    out = []
    for y in range(0, h, step):
        line = ""
        for x in range(0, w, step):
            R, G, B, A = img[y][x]
            if A == 0:
                line += " "
            elif R > 150 and R > G + 50 and R > B + 50:
                line += "#"
            elif R > 200 and G > 200 and B > 200:
                line += "."
            else:
                line += "@"
        out.append(line)
    return "\n".join(out)


def is_rust(px):
    R, G, B, A = px
    return A and R > 140 and R > G + 50 and R > B + 50


def profile(img, mask=0.0):
    """Outer rust radius and the radius where rust starts, per 5 degrees."""
    h = len(img); w = len(img[0])
    cx = cy = w / 2.0
    k = (2.0 * mask / w) if mask else (512.0 / w)
    outer, inner = [], []
    for i in range(72):
        a = 2 * math.pi * i / 72
        ro = 0
        for r in range(int(cx) - 1, 0, -1):
            x = int(cx + r * math.cos(a)); y = int(cy + r * math.sin(a))
            if 0 <= x < w and 0 <= y < h and is_rust(img[y][x]):
                ro = r * k
                break
        outer.append(ro)
        ri = None
        for r in range(1, int(cx)):
            x = int(cx + r * math.cos(a)); y = int(cy + r * math.sin(a))
            if 0 <= x < w and 0 <= y < h and is_rust(img[y][x]):
                ri = r * k
                break
        inner.append(ri if ri is not None else 0.0)
    return outer, inner


def main():
    global SCALE_ART
    if "--adaptive" in sys.argv:
        SCALE_ART = D.FG
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    path = args[0] if args else "/tmp/icon.png"
    px = int(args[1]) if len(args) > 1 else 512
    mask = float(args[2]) if len(args) > 2 else 0.0
    img = render(px, mask)
    write_png(path, img)
    print("wrote", path, px, "px, mask", mask)
    if "--ascii" in sys.argv and px <= 160:
        print(ascii_art(img, 1 if px <= 120 else 2))
    if px == 512:
        outer, inner = profile(img, mask)
        print("outer r: min %.1f max %.1f (want %.1f, all equal)"
              % (min(outer), max(outer), D.R_OUT))
        print("rust starts (from centre) r: alternates %.1f / %.1f"
              % (min(v for v in inner if v), max(inner)))
        word = [sh for sh in D.shapes_full() if sh.get("fill") == D.BG][0]
        pts = [p for poly in flatten(word["d"]) for p in poly]
        rad = max(math.hypot(x - D.CX, y - D.CY) for x, y in pts)
        print("word: bbox radius %.1f, tooth tips at %.1f" % (rad, D.R_TOOTH_IN))


if __name__ == "__main__":
    main()
