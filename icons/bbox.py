#!/usr/bin/env python3
"""Print each path of an SVG / VectorDrawable: number of subpaths, bbox."""
import sys
sys.path.insert(0, "/home/hematite/work/pdf-android/icons")
import raster_logo as R

for d, colour, eo in R.paths(sys.argv[1]):
    subs = R.flatten(d)
    pts = [p for s in subs for p in s]
    xs = [p[0] for p in pts]
    ys = [p[1] for p in pts]
    print("{:8s} {:>3} subpaths {:>5} pts  x {:8.2f}..{:8.2f}  y {:8.2f}..{:8.2f}"
          .format(colour, len(subs), len(pts), min(xs), max(xs), min(ys), max(ys)))
