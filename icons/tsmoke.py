#!/usr/bin/env python3
import sys
sys.path.insert(0, "/home/hematite/work/pdf-android/icons")
import logo_icon as L

print("smoke:", L.transform("m 54.004,0.68 l 10,0 z", 1.0, 100.0, 100.0))
print("smoke2:", L.transform("M 54.004,0.68 L 60,1 z", 2.0, 10.0, 20.0))
print("gear head:", L.transform(L.GEAR, 1.0, 0.0, 0.0)[:160])
print("gear head scaled:", L.transform(L.GEAR, 4.3207, 22.68, 22.68)[:200])
