#!/usr/bin/env python3
"""Minimal PNG reader + screen analysis, no PIL in this image.

    pngstat.py shot.png            # size, band means, page bbox, ink counts
    pngstat.py shot.png --bands 24 # how many horizontal bands to summarise

Colour types 0/2/4/6, bit depth 8, non-interlaced: what `screencap` writes.
"""
import sys
import zlib
import numpy as np


def read_png(path):
    data = open(path, "rb").read()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", "not a png"
    pos, idat, hdr, plte = 8, [], None, None
    while pos < len(data):
        ln = int.from_bytes(data[pos:pos + 4], "big")
        typ = data[pos + 4:pos + 8]
        body = data[pos + 8:pos + 8 + ln]
        pos += 12 + ln
        if typ == b"IHDR":
            hdr = body
        elif typ == b"IDAT":
            idat.append(body)
        elif typ == b"PLTE":
            plte = body
        elif typ == b"IEND":
            break
    w = int.from_bytes(hdr[0:4], "big")
    h = int.from_bytes(hdr[4:8], "big")
    depth, ctype, _, _, interlace = hdr[8], hdr[9], hdr[10], hdr[11], hdr[12]
    if interlace:
        raise SystemExit("interlaced png not supported")
    if depth not in (8, 16):
        raise SystemExit(f"bit depth {depth} not supported")
    chans = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[ctype]
    bpp = chans * (depth // 8)
    raw = zlib.decompress(b"".join(idat))
    stride = w * bpp
    out = np.zeros((h, stride), dtype=np.uint8)
    prev = np.zeros(stride, dtype=np.uint8)
    p = 0
    for y in range(h):
        f = raw[p]
        p += 1
        line = np.frombuffer(raw[p:p + stride], dtype=np.uint8).copy()
        p += stride
        if f == 0:
            cur = line
        elif f == 1:  # Sub
            cur = line
            for i in range(bpp, stride):
                cur[i] = (int(cur[i]) + int(cur[i - bpp])) & 0xFF
        elif f == 2:  # Up
            cur = (line.astype(np.int16) + prev).astype(np.uint8)
        elif f == 3:  # Average
            cur = line
            for i in range(stride):
                left = int(cur[i - bpp]) if i >= bpp else 0
                cur[i] = (int(cur[i]) + ((left + int(prev[i])) >> 1)) & 0xFF
        elif f == 4:  # Paeth
            cur = line
            for i in range(stride):
                a = int(cur[i - bpp]) if i >= bpp else 0
                b = int(prev[i])
                c = int(prev[i - bpp]) if i >= bpp else 0
                pa, pb, pc = abs(b - c), abs(a - c), abs(a + b - 2 * c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                cur[i] = (int(cur[i]) + pr) & 0xFF
        else:
            raise SystemExit(f"filter {f} unknown")
        out[y] = cur
        prev = cur
    if depth == 16:
        out = out.reshape(h, w, chans * 2)[:, :, ::2].reshape(h, w * chans)
        chans = chans
        bpp = chans
    px = out.reshape(h, w, bpp)
    if ctype == 3:
        px = np.frombuffer(plte, dtype=np.uint8).reshape(-1, 3)[px[:, :, 0]]
    elif ctype == 0:
        px = np.repeat(px, 3, axis=2)
    elif ctype == 4:
        px = np.repeat(px[:, :, :1], 3, axis=2)
    return px[:, :, :3].astype(np.int16)


def main():
    path = sys.argv[1]
    bands = 24
    if "--bands" in sys.argv:
        bands = int(sys.argv[sys.argv.index("--bands") + 1])
    px = read_png(path)
    h, w, _ = px.shape
    luma = ((299 * px[:, :, 0].astype(np.int32) + 587 * px[:, :, 1].astype(np.int32)
             + 114 * px[:, :, 2].astype(np.int32)) // 1000)
    print(f"{path}: {w}x{h} mean {luma.mean():.1f} std {luma.std():.1f}")
    bs = max(1, h // bands)
    print("bands (y0 mean min max):")
    for i in range(bands):
        b = luma[i * bs:(i + 1) * bs]
        print(f"  {i * bs:5d} {b.mean():6.1f} {b.min():4d} {b.max():4d}")
    # Bounding box of "paper": bright pixels
    bright = luma > 200
    if bright.any():
        ys, xs = np.where(bright)
        print(f"bright bbox x {xs.min()}..{xs.max()} y {ys.min()}..{ys.max()} "
              f"({bright.mean() * 100:.1f}% of px)")
    dark = luma < 60
    if dark.any():
        ys, xs = np.where(dark)
        print(f"dark bbox   x {xs.min()}..{xs.max()} y {ys.min()}..{ys.max()} "
              f"({dark.mean() * 100:.1f}% of px)")
    # Column profile of the middle band, to spot where the page edges are.
    mid = luma[h // 3: 2 * h // 3]
    cols = mid.mean(axis=0)
    step = max(1, w // 20)
    print("col means (x0 value):", " ".join(f"{x}:{cols[x]:.0f}" for x in range(0, w, step)))


if __name__ == "__main__":
    main()
