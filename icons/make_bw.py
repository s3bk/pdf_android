#!/usr/bin/env python3
"""icon-spur36.svg, in black on white.

    python3 make_bw.py

Writes icon-bw.svg (the file to hand to someone) and icon-bw-512.png (a
preview).  raster.py has no <rect>, so the preview is drawn from a temporary
copy whose background rect is a circle big enough to cover the square.
"""
import re

SRC = "icon-spur36.svg"
SVG = "icon-bw.svg"
PNG = "icon-bw-512.png"

svg = open(SRC).read()
svg = svg.replace('<rect width="512" height="512" fill="#1b2b3a"/>',
                  '<rect width="512" height="512" fill="#ffffff"/>')
svg = svg.replace("#e43717", "#000000")   # the gear body: was the rust ring
svg = svg.replace("#1b2b3a", "#000000")   # the letters: were the navy of the old ground
open(SVG, "w").write(svg)

import raster
tmp = re.sub(r'<rect width="512" height="512" fill="#ffffff"/>',
             '<circle cx="256" cy="256" r="362.04" fill="#ffffff"/>', svg)
open("/tmp/icon-bw-preview.svg", "w").write(tmp)
raster.main("/tmp/icon-bw-preview.svg", PNG, 512, 2)
print("wrote", SVG, "and", PNG)
