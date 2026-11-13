#!/usr/bin/env python3
"""Rasterise the icon straight from the emitted path data - no browser, no
librsvg, no Android.  The point is to see exactly the coordinates that went
into the APK: if the VectorDrawable is wrong, this picture is wrong the same
way.

    raster_logo.py <file.svg|file.xml> <out.png> [size] [evenodd]

Supports M/L/H/V/C/Q/Z, absolute and relative, flattening curves into short
lines; fills with the nonzero rule (or even-odd when asked, which is what the
monochrome layer uses).  3x3 supersampling.
"""
import math
import re
import sys

TOK = re.compile(r"[A-Za-z]|-?\d*\.?\d+(?:[eE][-+]?\d+)?")
NARG = dict(M=2, L=2, H=1, V=1, C=6, S=4, Q=4, T=2, Z=0, A=7)


def arc_points(x1, y1, rx, ry, phi_deg, fa, fs, x2, y2, steps):
    """SVG endpoint arc -> polyline, the usual centre parameterisation."""
    if rx == 0 or ry == 0 or (x1 == x2 and y1 == y2):
        return [(x2, y2)]
    phi = math.radians(phi_deg % 360.0)
    cosp, sinp = math.cos(phi), math.sin(phi)
    dx2, dy2 = (x1 - x2) / 2.0, (y1 - y2) / 2.0
    x1p = cosp * dx2 + sinp * dy2
    y1p = -sinp * dx2 + cosp * dy2
    rx, ry = abs(rx), abs(ry)
    lam = x1p * x1p / (rx * rx) + y1p * y1p / (ry * ry)
    if lam > 1:
        s = math.sqrt(lam)
        rx *= s
        ry *= s
    num = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p
    den = rx * rx * y1p * y1p + ry * ry * x1p * x1p
    co = math.sqrt(max(0.0, num / den)) if den else 0.0
    if fa == fs:
        co = -co
    cxp = co * rx * y1p / ry
    cyp = -co * ry * x1p / rx
    cx = cosp * cxp - sinp * cyp + (x1 + x2) / 2.0
    cy = sinp * cxp + cosp * cyp + (y1 + y2) / 2.0

    def ang(ux, uy, vx, vy):
        n = math.hypot(ux, uy) * math.hypot(vx, vy)
        a = math.acos(max(-1.0, min(1.0, (ux * vx + uy * vy) / n))) if n else 0.0
        return -a if ux * vy - uy * vx < 0 else a

    ux, uy = (x1p - cxp) / rx, (y1p - cyp) / ry
    vx, vy = (-x1p - cxp) / rx, (-y1p - cyp) / ry
    th1 = ang(1, 0, ux, uy)
    dth = ang(ux, uy, vx, vy)
    if not fs and dth > 0:
        dth -= 2 * math.pi
    if fs and dth < 0:
        dth += 2 * math.pi
    pts = []
    for k in range(1, steps + 1):
        t = th1 + dth * k / steps
        pts.append((cx + cosp * rx * math.cos(t) - sinp * ry * math.sin(t),
                    cy + sinp * rx * math.cos(t) + cosp * ry * math.sin(t)))
    return pts


def flatten(d, steps=12):
    """Path data -> list of closed polylines (in the file's own units)."""
    subs, cur = [], []
    toks = TOK.findall(d)
    i, x, y, sx, sy = 0, 0.0, 0.0, 0.0, 0.0
    cmd = None
    while i < len(toks):
        t = toks[i]
        if t.isalpha():
            cmd = t
            i += 1
            if cmd in "Zz":
                if cur:
                    subs.append(cur)
                cur, x, y = [], sx, sy
                continue
        c = cmd.upper()
        rel = cmd.islower()
        n = NARG[c]
        a = [float(v) for v in toks[i:i + n]]
        i += n
        if c == "M":
            x, y = (x + a[0], y + a[1]) if rel else (a[0], a[1])
            if cur:
                subs.append(cur)
            cur = [(x, y)]
            sx, sy = x, y
            cmd = "l" if rel else "L"
        elif c == "L":
            x, y = (x + a[0], y + a[1]) if rel else (a[0], a[1])
            cur.append((x, y))
        elif c == "H":
            x = x + a[0] if rel else a[0]
            cur.append((x, y))
        elif c == "V":
            y = y + a[0] if rel else a[0]
            cur.append((x, y))
        elif c in "QT":
            cx, cy = (x + a[0], y + a[1]) if rel else (a[0], a[1])
            ex, ey = (x + a[2], y + a[3]) if rel else (a[2], a[3])
            for k in range(1, steps + 1):
                s = k / steps
                cur.append(((1 - s) ** 2 * x + 2 * (1 - s) * s * cx + s * s * ex,
                            (1 - s) ** 2 * y + 2 * (1 - s) * s * cy + s * s * ey))
            x, y = ex, ey
        elif c == "A":
            e = (x + a[5], y + a[6]) if rel else (a[5], a[6])
            for px, py in arc_points(x, y, a[0], a[1], a[2], int(a[3]), int(a[4]),
                                    e[0], e[1], steps * 3):
                cur.append((px, py))
            x, y = e
        elif c in "CS":
            p0 = (x, y)
            if c == "C":
                c1 = (x + a[0], y + a[1]) if rel else (a[0], a[1])
                c2 = (x + a[2], y + a[3]) if rel else (a[2], a[3])
                e = (x + a[4], y + a[5]) if rel else (a[4], a[5])
            else:
                raise SystemExit("S not needed here")
            for k in range(1, steps + 1):
                s = k / steps
                m = 1 - s
                cur.append((m ** 3 * p0[0] + 3 * m * m * s * c1[0]
                            + 3 * m * s * s * c2[0] + s ** 3 * e[0],
                            m ** 3 * p0[1] + 3 * m * m * s * c1[1]
                            + 3 * m * s * s * c2[1] + s ** 3 * e[1]))
            x, y = e
        else:
            raise SystemExit("unsupported " + cmd)
    if cur:
        subs.append(cur)
    return subs


def paths(path):
    """[(pathData, colour, evenodd)] in document order."""
    src = open(path).read()
    out = []
    for m in re.finditer(r"<path\b([^>]*)>", src, re.S):
        at = m.group(1)
        d = re.search(r'(?:\s|^)(?:android:)?(?:pathData|d)="([^"]+)"', at)
        if not d:
            continue
        colour = re.search(r'fill="(#[0-9a-fA-F]{6})"', at) or \
            re.search(r'android:fillColor="(#[0-9a-fA-F]{6})"', at)
        eo = ("evenOdd" in at) or ('fill-rule="evenodd"' in at)
        out.append((d.group(1), colour.group(1) if colour else "#000000", eo))
    return out


def hexrgb(h):
    return (int(h[1:3], 16), int(h[3:5], 16), int(h[5:7], 16))


def fill(img, W, ss, subs, colour, evenodd):
    """Scanline fill; `subs` are in canvas units (512), W is the supersampled
    pixel width, so one canvas unit is W/512 pixels."""
    sc = W / 512.0
    edges = []
    for loop in subs:
        n = len(loop)
        for k in range(n):
            (x0, y0), (x1, y1) = loop[k], loop[(k + 1) % n]
            if y0 == y1:
                continue
            edges.append((x0, y0, x1, y1, 1 if y1 > y0 else -1))
    for row in range(W):
        yc = (row + 0.5) / sc
        xs = []
        for x0, y0, x1, y1, d in edges:
            lo, hi = (y0, y1) if y0 < y1 else (y1, y0)
            if lo <= yc < hi:
                xs.append((x0 + (yc - y0) / (y1 - y0) * (x1 - x0), d))
        if not xs:
            continue
        xs.sort()
        wind, parity = 0, 0
        for idx in range(len(xs) - 1):
            wind += xs[idx][1]
            parity ^= 1
            inside = (parity == 1) if evenodd else (wind != 0)
            if not inside:
                continue
            a = max(0, int(math.ceil(xs[idx][0] * sc - 0.5)))
            b = min(W, int(math.ceil(xs[idx + 1][0] * sc - 0.5)))
            base = row * W
            for col in range(a, b):
                p = (base + col) * 3
                img[p], img[p + 1], img[p + 2] = colour


def png(path, buf, size):
    import struct
    import zlib
    raw = b"".join(b"\x00" + bytes(buf[y * size * 3:(y + 1) * size * 3])
                   for y in range(size))

    def chunk(tag, data):
        return (struct.pack(">I", len(data)) + tag + data
                + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF))
    hdr = struct.pack(">IIBBBBB", size, size, 8, 2, 0, 0, 0)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", hdr)
                + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def main():
    src, out = sys.argv[1], sys.argv[2]
    size = int(sys.argv[3]) if len(sys.argv) > 3 else 512
    force_eo = len(sys.argv) > 4 and sys.argv[4] == "evenodd"
    ss = 3
    W = size * ss
    img = bytearray(b"\xff\xff\xff" * (W * W))     # white ground
    k = W / 512.0
    for d, colour, eo in paths(src):
        subs = [[(x, y) for x, y in loop] for loop in flatten(d)]
        fill(img, W, ss, subs, hexrgb(colour), eo or force_eo)
    buf = bytearray(size * size * 3)
    for y in range(size):
        for x in range(size):
            r = g = b = 0
            for dy in range(ss):
                row = (y * ss + dy) * W
                for dx in range(ss):
                    p = (row + x * ss + dx) * 3
                    r += img[p]
                    g += img[p + 1]
                    b += img[p + 2]
            q = (y * size + x) * 3
            buf[q], buf[q + 1], buf[q + 2] = r // 9, g // 9, b // 9
    png(out, buf, size)
    print("wrote", out, size, "px from", src)


if __name__ == "__main__":
    main()
