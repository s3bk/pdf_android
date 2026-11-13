#!/usr/bin/env python3
"""Rasterise the icon SVG without a browser.

The icon is only circles and M/L/H/V/Q/Z paths, so a scanline filler with
2x2 supersampling is enough to see it - no librsvg, no chrome.
"""
import math
import re
import sys
import zlib
import struct

TOK = re.compile(r"[A-Za-z]|-?\d*\.?\d+(?:[eE][-+]?\d+)?")


def parse(d):
    """Subpaths as lists of points, quads flattened."""
    subs, cur = [], []
    toks = TOK.findall(d)
    i, x, y = 0, 0.0, 0.0
    cmd = None
    start = (0.0, 0.0)
    while i < len(toks):
        t = toks[i]
        if t.isalpha():
            cmd = t
            i += 1
            if cmd in "Zz":
                if cur:
                    subs.append(cur)
                cur = []
                x, y = start
                continue
        n = {"M": 2, "L": 2, "H": 1, "V": 1, "Q": 4, "C": 6, "T": 2, "S": 4}[cmd.upper()]
        args = [float(v) for v in toks[i:i + n]]
        i += n
        c = cmd.upper()
        if c == "M":
            if cur:
                subs.append(cur)
            x, y = args
            start = (x, y)
            cur = [(x, y)]
            cmd = "L" if cmd == "M" else "l"   # implicit lineto after moveto
        elif c == "L":
            if cmd.islower():
                x, y = x + args[0], y + args[1]
            else:
                x, y = args
            cur.append((x, y))
        elif c == "H":
            x = x + args[0] if cmd.islower() else args[0]
            cur.append((x, y))
        elif c == "V":
            y = y + args[0] if cmd.islower() else args[0]
            cur.append((x, y))
        elif c == "Q":
            if cmd.islower():
                cx, cy = x + args[0], y + args[1]
                ex, ey = x + args[2], y + args[3]
            else:
                cx, cy, ex, ey = args
            for k in range(1, 9):
                s = k / 8.0
                bx = (1 - s) ** 2 * x + 2 * (1 - s) * s * cx + s * s * ex
                by = (1 - s) ** 2 * y + 2 * (1 - s) * s * cy + s * s * ey
                cur.append((bx, by))
            x, y = ex, ey
        else:
            raise SystemExit("unsupported command " + cmd)
    if cur:
        subs.append(cur)
    return subs


def fill(img, W, H, subs, colour, ss):
    """Nonzero fill of `subs` onto the supersampled RGB buffer."""
    # scanline over supersampled rows
    edges = []
    for loop in subs:
        n = len(loop)
        for k in range(n):
            (x0, y0), (x1, y1) = loop[k], loop[(k + 1) % n]
            if y0 == y1:
                continue
            d = 1 if y1 > y0 else -1
            edges.append((x0, y0, x1, y1, d))
    for row in range(H):
        yc = (row + 0.5) / ss
        xs = []
        for x0, y0, x1, y1, d in edges:
            lo, hi = (y0, y1) if y0 < y1 else (y1, y0)
            if lo <= yc < hi:
                t = (yc - y0) / (y1 - y0)
                xs.append((x0 + t * (x1 - x0), d))
        if not xs:
            continue
        xs.sort()
        wind = 0
        for idx in range(len(xs) - 1):
            wind += xs[idx][1]
            if wind == 0:
                continue
            a = max(0, int(math.ceil(xs[idx][0] * ss - 0.5)))
            b = min(W, int(math.ceil(xs[idx + 1][0] * ss - 0.5)))
            base = row * W
            for col in range(a, b):
                p = (base + col) * 3
                img[p] = colour[0]
                img[p + 1] = colour[1]
                img[p + 2] = colour[2]


def circle(img, W, ss, cx, cy, r, colour):
    """Canvas units in, supersampled buffer out (it applies `ss` itself)."""
    x0, x1 = int((cx - r) * ss) - 1, int((cx + r) * ss) + 2
    y0, y1 = int((cy - r) * ss) - 1, int((cy + r) * ss) + 2
    for row in range(max(0, y0), min(W, y1)):
        yy = (row + 0.5) / ss - cy
        if abs(yy) > r:
            continue
        half = math.sqrt(max(0.0, r * r - yy * yy))
        a = max(0, int(math.ceil((cx - half) * ss - 0.5)))
        b = min(W, int(math.ceil((cx + half) * ss - 0.5)))
        base = row * W
        for col in range(a, b):
            p = (base + col) * 3
            img[p] = colour[0]
            img[p + 1] = colour[1]
            img[p + 2] = colour[2]


def png(path, rgba, W):
    raw = b"".join(b"\x00" + bytes(rgba[y * W * 3:(y + 1) * W * 3]) for y in range(len(rgba) // (W * 3)))
    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        return c + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
    hdr = struct.pack(">IIBBBBB", W, len(raw) // (W * 3 + 1), 8, 2, 0, 0, 0)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", hdr)
                + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def main(svg_path, out, size=512, ss=2, mask=None):
    svg = open(svg_path).read()
    W = size * ss
    img = bytearray(b"\x20\x22\x27" * (W * W))     # canvas colour
    # Document order matters: pain on top is later in the file, and the SVG
    # that this reads paints the white bore OVER the gear body.  One pass over
    # both element kinds, in the order they appear.
    el = re.compile(
        r'<circle cx="([\d.]+)" cy="([\d.]+)" r="([\d.]+)" fill="(#[0-9a-fA-F]{6})"/>'
        r'|<path d="([^"]+)" fill="(#[0-9a-fA-F]{6})"')
    for m in el.finditer(svg):
        if m.group(1) is not None:
            cx, cy, r, col = (float(m.group(1)), float(m.group(2)),
                              float(m.group(3)), m.group(4))
            circle(img, W, ss, cx, cy, r, hexrgb(col))
        else:
            fill(img, W, ss_bound(W), parse(m.group(5)), hexrgb(m.group(6)), ss)
    # downsample
    out_buf = bytearray(size * size * 3)
    for y in range(size):
        for x in range(size):
            r = g = b = 0
            for dy in range(ss):
                row = (y * ss + dy) * W
                for dx in range(ss):
                    p = (row + x * ss + dx) * 3
                    if mask is not None:
                        mx = (x * ss + dx) / ss - size / 2.0
                        my = (y * ss + dy) / ss - size / 2.0
                        if mx * mx + my * my > mask * mask:
                            continue
                    r += img[p]
                    g += img[p + 1]
                    b += img[p + 2]
            q = (y * size + x) * 3
            out_buf[q], out_buf[q + 1], out_buf[q + 2] = r // (ss * ss), g // (ss * ss), b // (ss * ss)
    png(out, out_buf, size)
    print("wrote", out, size, "px")


def ss_bound(W):
    return W


def hexrgb(h):
    return (int(h[1:3], 16), int(h[3:5], 16), int(h[5:7], 16))


if __name__ == "__main__":
    a = sys.argv[1:]
    path = a[0] if a else "icon.svg"
    out = a[1] if len(a) > 1 else "icon-raster.png"
    size = int(a[2]) if len(a) > 2 else 512
    mask = float(a[3]) if len(a) > 3 else None
    main(path, out, size, 2, mask)
