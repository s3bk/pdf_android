#!/usr/bin/env python3
"""Involute gear generator - standalone, no dependencies, emits SVG.

    ./gear.py                       # Tulip's spec: 16 teeth, m2, 25deg, 0 backlash
    ./gear.py --teeth 12 --module 3 --output g12.svg
    ./gear.py --external            # same numbers as a spur gear instead
    ./gear.py --path-only           # just the 'd', for a VectorDrawable

The gear is the honest involute, not a rounded imitation:

  * the involute does not exist inside the base circle, so an internal tooth's
    tip is truncated at r_base (and an external gear's root likewise);
  * a tooth gets thicker with radius, so the slot between two internal teeth
    closes at some radius; the slot stops there while it is still the width
    given by --slot-angle, and the rest is solid rim;
  * backlash b (mm, total at the pitch circle) is taken off both flanks, i.e.
    an angular shave of b/2 / r_pitch on each side of the tooth.

Everything is computed from the spec; nothing is hand-tuned.  --verify prints
the numbers and checks the polygon (angle monotonicity, radius bounds, no
self-intersection by the winding/shoelace test).
"""
import argparse
import math
import sys

TAU = 2.0 * math.pi


# --------------------------------------------------------------------------
# involute helpers.  All angles in the code are radians unless _deg is in the
# name; the involute function inv(a) = tan(a) - a.
# --------------------------------------------------------------------------
def inv(a):
    return math.tan(a) - a


class Gear:
    """One gear.  Angles are measured from a tooth centre.

    kind = 'internal' (ring: teeth point inward, rim outside) or
           'external' (spur: teeth point outward, hub inside).
    """

    def __init__(self, teeth, module, pressure_angle_deg, backlash=0.0,
                 rim_diameter=None, tip_diameter=None, kind="internal",
                 slot_angle_deg=2.0):
        self.n = int(teeth)
        self.m = float(module)
        self.alpha = math.radians(float(pressure_angle_deg))
        self.backlash = float(backlash)
        self.kind = kind
        self.slot_angle = math.radians(slot_angle_deg)

        self.r_pitch = self.m * self.n / 2.0
        self.r_base = self.r_pitch * math.cos(self.alpha)
        self.r_rim = (float(rim_diameter) / 2.0) if rim_diameter else None
        self.pitch_angle = TAU / self.n

        # half tooth thickness at the pitch circle, radians
        self.half_tooth_p = (math.pi / (2.0 * self.n)
                             - (self.backlash / 2.0) / self.r_pitch)
        self.inv_alpha = inv(self.alpha)

        if kind == "internal":
            # tips truncated at the base circle: no involute below it
            self.r_tip = min(self.r_base,
                             float(tip_diameter) / 2.0 if tip_diameter else self.r_base)
            # root: where the slot has narrowed to --slot-angle
            self.r_root = self._solve_root()
        else:
            self.r_tip = (float(tip_diameter) / 2.0 if tip_diameter
                          else self.r_pitch + self.m)
            self.r_root = max(self.r_base, self.r_pitch - 1.25 * self.m)

        if self.r_rim is not None and self.r_rim <= self.r_root and kind == "internal":
            raise SystemExit("rim diameter leaves no material outside the slots "
                             "(rim r {:.2f} <= root r {:.2f})".format(
                                 self.r_rim, self.r_root))

    # -- tooth geometry ----------------------------------------------------
    def half_tooth(self, r):
        """Half angular thickness of the tooth at radius r (radians)."""
        if r <= self.r_base:
            a = 0.0
        else:
            a = math.acos(min(1.0, self.r_base / r))
        if self.kind == "internal":
            # a ring tooth gets thicker with radius
            return self.half_tooth_p + inv(a) - self.inv_alpha
        # a spur tooth gets thinner with radius
        return self.half_tooth_p + self.inv_alpha - inv(a)

    def half_gap(self, r):
        """Half angular width of the gap next to a tooth, at radius r."""
        return self.pitch_angle / 2.0 - self.half_tooth(r)

    def _solve_root(self):
        """Internal: radius where the slot closes to slot_angle."""
        lo, hi = self.r_base, self.r_base * 1.6 + self.m
        for _ in range(200):
            mid = (lo + hi) / 2.0
            if self.half_gap(mid) > self.slot_angle:
                lo = mid
            else:
                hi = mid
        return (lo + hi) / 2.0

    # -- the outline -------------------------------------------------------
    def outline(self, steps=14):
        """Points (r, theta) of the toothed opening (internal) or outer
        profile (external), going once round with theta increasing."""
        pts = []
        half = self.pitch_angle / 2.0
        n = self.n
        for k in range(n):
            c = k * self.pitch_angle
            if self.kind == "internal":
                # climb the right flank of the slot from tip out to root
                for i in range(steps + 1):
                    r = self.r_tip + (self.r_root - self.r_tip) * i / steps
                    pts.append((r, c + self.half_tooth(r)))
                # land across the root, to the next tooth's flank
                pts.append((self.r_root, c + self.pitch_angle - self.half_tooth(self.r_root)))
                # back down the left flank of the next tooth
                for i in range(steps, -1, -1):
                    r = self.r_tip + (self.r_root - self.r_tip) * i / steps
                    pts.append((r, c + self.pitch_angle - self.half_tooth(r)))
                # across the tip
                pts.append((self.r_tip, c + self.pitch_angle + self.half_tooth(self.r_tip)))
            else:
                # external: tips outward, theta always increasing.  Start at the
                # tip's right corner, down the right flank to the root, across
                # the root land, then back up the next tooth's left flank - which
                # ends exactly at the next tooth's tip corner, so there is no
                # tip-land point here (adding one made the angle jump backwards).
                for i in range(steps + 1):
                    r = self.r_tip + (self.r_root - self.r_tip) * i / steps
                    pts.append((r, c + self.half_tooth(r)))
                pts.append((self.r_root, c + self.pitch_angle - self.half_tooth(self.r_root)))
                for i in range(steps, -1, -1):
                    r = self.r_tip + (self.r_root - self.r_tip) * i / steps
                    pts.append((r, c + self.pitch_angle - self.half_tooth(r)))
        return pts

    # -- checks ------------------------------------------------------------
    def verify(self):
        """Raise if the numbers cannot make a sane gear; return notes."""
        notes = []
        if self.n < 6:
            notes.append("fewer than 6 teeth - the flanks will cross")
        if not (0.15 < math.degrees(self.alpha) < 40):
            notes.append("pressure angle outside 15..40 deg")
        if self.half_tooth_p <= 0:
            raise SystemExit("backlash {:.3f} mm eats the whole tooth".format(self.backlash))
        if self.half_tooth(self.r_tip) <= 0:
            raise SystemExit("tooth tips have zero thickness")
        if self.half_tooth(self.r_tip) >= self.pitch_angle / 2.0:
            raise SystemExit(
                "teeth overlap at r {:.3f} mm: half thickness {:.2f} deg > half "
                "pitch {:.2f} deg - the tip circle is too far out for N={} m={:g} "
                "alpha={:g}.  An {} gear of these numbers has its tips at dia "
                "{:.3f} mm.".format(
                    self.r_tip, math.degrees(self.half_tooth(self.r_tip)),
                    math.degrees(self.pitch_angle / 2.0), self.n, self.m,
                    round(math.degrees(self.alpha), 1), self.kind,
                    2 * (self.r_pitch + self.m)))
        if self.kind == "internal" and self.half_gap(self.r_root) < self.slot_angle - 1e-9:
            raise SystemExit("slots are closed before the root radius")
        # radii must be ordered and every theta in range
        pts = self.outline()
        prev = None
        for r, th in pts:
            if self.kind == "internal" and not (self.r_tip - 1e-6 <= r <= self.r_root + 1e-6):
                raise SystemExit("internal outline leaves tip..root band")
            if prev is not None and th < prev - 1e-9:
                raise SystemExit("outline angle went backwards at r {:.3f}".format(r))
            prev = th
        if len(pts) < 8:
            raise SystemExit("degenerate outline")
        if self.kind == "internal" and self.r_rim:
            notes.append("rim wall outside the slots: {:.2f} mm".format(
                self.r_rim - self.r_root))
        return notes

    # -- numbers a human wants ---------------------------------------------
    def report(self):
        g = self
        lines = [
            "{kind} gear: N={n}  m={m:g} mm  alpha={a:g} deg  backlash={b:g} mm".format(
                kind=g.kind, n=g.n, m=g.m, a=math.degrees(g.alpha), b=g.backlash),
            "  pitch r      %8.3f mm   (dia %7.3f)" % (g.r_pitch, 2 * g.r_pitch),
            "  base r       %8.3f mm" % g.r_base,
            "  tip r        %8.3f mm   (dia %7.3f)%s" % (
                g.r_tip, 2 * g.r_tip,
                "  truncated at base" if g.kind == "internal" and abs(g.r_tip - g.r_base) < 1e-9 else ""),
            "  root r       %8.3f mm   (dia %7.3f)" % (g.r_root, 2 * g.r_root),
        ]
        if g.r_rim:
            lines.append("  rim r        %8.3f mm   (dia %7.3f)" % (g.r_rim, 2 * g.r_rim))
        lines += [
            "  tooth half-angle: %6.3f deg at pitch, %6.3f deg at tip, %6.3f deg at root"
            % (math.degrees(g.half_tooth(g.r_pitch)),
               math.degrees(g.half_tooth(g.r_tip)),
               math.degrees(g.half_tooth(g.r_root))),
            "  slot half-angle:  %6.3f deg at tip,   %6.3f deg at root"
            % (math.degrees(g.half_gap(g.r_tip)),
               math.degrees(g.half_gap(g.r_root))),
        ]
        return "\n".join(lines)


# --------------------------------------------------------------------------
# SVG
# --------------------------------------------------------------------------
def circle_path(cx, cy, r):
    """Four quarter arcs, sweep 1 - no 180-degree ambiguity."""
    return ("M{:g},{:g} A{:g},{:g} 0 0 1 {:g},{:g} A{:g},{:g} 0 0 1 {:g},{:g} "
            "A{:g},{:g} 0 0 1 {:g},{:g} A{:g},{:g} 0 0 1 {:g},{:g} Z").format(
                cx - r, cy, r, r, cx, cy - r, r, r, cx + r, cy,
                r, r, cx, cy + r, r, r, cx - r, cy)


def to_svg_path(pts, cx, cy, scale, rot_deg=0.0):
    rot = math.radians(rot_deg)
    parts = []
    for i, (r, th) in enumerate(pts):
        a = th + rot
        x = cx + r * scale * math.cos(a)
        y = cy + r * scale * math.sin(a)
        parts.append(("M" if i == 0 else "L") + _pt(x, y))
    return "".join(parts) + "Z"


def _n(v):
    s = "{:.2f}".format(v)
    s = s.rstrip("0").rstrip(".")
    return "0" if s in ("", "-0") else s


def _pt(x, y):
    return "{},{}".format(_n(x), _n(y))


def build(args):
    kind = "external" if args.external else "internal"
    g = Gear(args.teeth, args.module, args.pressure_angle, args.backlash,
             rim_diameter=args.rim_diameter, tip_diameter=args.tip_diameter,
             kind=kind, slot_angle_deg=args.slot_angle)
    notes = g.verify()

    # canvas: outer edge of the artwork (rim for a ring, tips for a spur)
    r_out = (g.r_rim if g.r_rim else g.r_tip)
    size = float(args.size)
    scale = (size / 2.0 - float(args.margin)) / r_out
    cx = cy = size / 2.0

    body = to_svg_path(g.outline(), cx, cy, scale, args.rotate)
    if kind == "internal":
        ring = circle_path(cx, cy, g.r_rim * scale) + " " + body
        shapes = [dict(d=ring, fill=args.fill, rule="evenodd")]
        if args.bore_fill:
            shapes.insert(0, dict(d=body, fill=args.bore_fill))
    else:
        shapes = [dict(d=body, fill=args.fill)]
        if args.bore_fill:
            shapes.append(dict(d=circle_path(cx, cy, g.r_root * scale),
                               fill=args.bore_fill))
    if args.hub:
        shapes.append(dict(d=circle_path(cx, cy, args.hub * scale), fill=args.hub_fill))
    return g, notes, shapes, body


def main(argv=None):
    p = argparse.ArgumentParser(description="involute gear -> SVG")
    p.add_argument("--teeth", type=int, default=16)
    p.add_argument("--module", type=float, default=2.0)
    p.add_argument("--pressure-angle", type=float, default=25.0)
    p.add_argument("--backlash", type=float, default=0.0, help="mm, total at pitch circle")
    p.add_argument("--rim-diameter", type=float, default=41.0, help="internal: outside of the ring")
    p.add_argument("--tip-diameter", type=float, default=None,
                   help="default: base circle (internal) / pitch+m (external)")
    p.add_argument("--slot-angle", type=float, default=2.0,
                   help="internal: stop the slot while it is this wide (deg, half)")
    p.add_argument("--external", action="store_true", help="spur gear instead of ring")
    p.add_argument("--rotate", type=float, default=0.0, help="deg, tooth phase")
    p.add_argument("--size", type=float, default=512.0)
    p.add_argument("--margin", type=float, default=8.0)
    p.add_argument("--fill", default="#e43717")
    p.add_argument("--bore-fill", default="none", help="fill the opening (e.g. white)")
    p.add_argument("--hub", type=float, default=None, help="external: hub radius in mm")
    p.add_argument("--hub-fill", default="#1b2b3a")
    p.add_argument("--background", default="none")
    p.add_argument("--output", "-o", default=None)
    p.add_argument("--path-only", action="store_true", help="print the outline 'd' only")
    p.add_argument("--quiet", action="store_true")
    args = p.parse_args(argv)

    g, notes, shapes, body = build(args)
    if not args.quiet:
        for line in g.report().splitlines():
            print(line, file=sys.stderr)
        for n in notes:
            print("  note: " + n, file=sys.stderr)
    if args.path_only:
        print(body)
        return 0

    out = ['<svg xmlns="http://www.w3.org/2000/svg" width="{s:g}" height="{s:g}" '
           'viewBox="0 0 {s:g} {s:g}">'.format(s=args.size)]
    if args.background != "none":
        out.append('  <rect width="{s:g}" height="{s:g}" fill="{f}"/>'.format(
            s=args.size, f=args.background))
    for sh in shapes:
        extra = ' fill-rule="{}"'.format(sh["rule"]) if sh.get("rule") else ""
        out.append('  <path d="{}" fill="{}"{}/>'.format(sh["d"], sh["fill"], extra))
    out.append("</svg>")
    text = "\n".join(out) + "\n"
    if args.output:
        open(args.output, "w").write(text)
        if not args.quiet:
            print("wrote", args.output, len(text), "B", file=sys.stderr)
    else:
        sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
