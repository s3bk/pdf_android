#!/usr/bin/env python3
"""Make the distributable APK: run build.sh with a release version bump, then
check the signature and copy the result to a versioned file name.

    python3 mkrelease.py 0.3 3              # version name, version code, both ABIs
    python3 mkrelease.py 0.3 3 arm64-v8a    # one ABI: a much smaller APK
"""
import os
import shutil
import subprocess
import sys

VERSION = sys.argv[1] if len(sys.argv) > 1 else "0.3"
CODE = sys.argv[2] if len(sys.argv) > 2 else "3"
# One ABI is what phones want: x86_64 is only the emulator, and a second copy of
# the 9 MB .so doubles the APK for nobody.
ABIS = sys.argv[3] if len(sys.argv) > 3 else "arm64-v8a,x86_64"

ROOT = "/home/hematite/work/pdf-android"
SDK = os.environ.get("ANDROID_SDK", "/home/hematite/android-sdk")
BT = f"{SDK}/build-tools/36.0.0"

env = dict(os.environ, VERSION_NAME=VERSION, VERSION_CODE=CODE, ABI=ABIS)
r = subprocess.run(["bash", "build.sh"], cwd=ROOT, env=env,
                   capture_output=True, text=True)
print(r.stdout[-4000:])
print(r.stderr[-4000:], file=sys.stderr)
if r.returncode != 0:
    sys.exit(f"build failed: {r.returncode}")

out = f"{ROOT}/target/android/pdf-viewer.apk"
suffix = "" if "," in ABIS else f"-{ABIS}"
final = f"{ROOT}/target/android/pdf-viewer-{VERSION}{suffix}.apk"
shutil.copy2(out, final)

# The signature is what lets an update install over the previous build, so
# check it rather than assume the apksigner line above worked.
v = subprocess.run(
    [f"{BT}/apksigner", "verify", "--print-certs", final],
    env=dict(env, JAVA_HOME=f"{SDK}/jdk17", PATH=f"{SDK}/jdk17/bin:" + env["PATH"]),
    capture_output=True, text=True)
print(v.stdout.strip() or v.stderr.strip())
if v.returncode != 0:
    sys.exit("apksigner verify failed")

b = subprocess.run(
    [f"{BT}/aapt2", "dump", "badging", final],
    capture_output=True, text=True)
for line in b.stdout.splitlines():
    if line.startswith("package:") or "native-code" in line or "sdkVersion" in line:
        print(line)

print(f"== {final}")
print("   bytes", os.path.getsize(final))
print("   ", subprocess.run(["openssl", "dgst", "-sha256", final],
                           capture_output=True, text=True).stdout.strip())
