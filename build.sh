#!/bin/bash
# Build the APK without gradle: cargo for the .so, aapt2 for the manifest,
# zipalign and apksigner for the packaging. Everything lands in target/android.
#
#   ./build.sh                  # arm64-v8a and x86_64 in one APK
#   ABI=arm64-v8a ./build.sh    # just the phone
set -euo pipefail

cd "$(dirname "$0")"

SDK=${ANDROID_SDK:-/home/hematite/android-sdk}
NDK=$SDK/android-ndk-r29
BT=$SDK/build-tools/36.0.0
PLATFORM=$SDK/platforms/android-36/android.jar
TC=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin
API=26
ABIS=${ABI:-arm64-v8a,x86_64}

OUT=target/android
mkdir -p "$OUT"

target_of() {
    case $1 in
        arm64-v8a) echo aarch64-linux-android ;;
        x86_64) echo x86_64-linux-android ;;
        *) echo "unsupported ABI $1" >&2; exit 1 ;;
    esac
}

# Per-ABI codegen. arm64-v8a is the build that ends up on a phone, so it gets
# the whole performance budget: NEON is baseline on armv8-a but is asked for
# explicitly (so a toolchain default cannot take it away), and the checks the
# release profile already drops are dropped by name here too. x86_64 is the
# emulator and stays plain.
rustflags_of() {
    case $1 in
        arm64-v8a) echo "-C target-feature=+neon -C debug-assertions=off -C overflow-checks=off" ;;
        *) echo "" ;;
    esac
}

echo "== cargo ($ABIS)"
for abi in ${ABIS//,/ }; do
    target=$(target_of "$abi")
    env \
        RUSTFLAGS="$(rustflags_of "$abi")${RUSTFLAGS:+ $RUSTFLAGS}" \
        CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$TC/aarch64-linux-android$API-clang \
        CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER=$TC/x86_64-linux-android$API-clang \
        CC_aarch64_linux_android=$TC/aarch64-linux-android$API-clang \
        CC_x86_64_linux_android=$TC/x86_64-linux-android$API-clang \
        AR_aarch64_linux_android=$TC/llvm-ar \
        AR_x86_64_linux_android=$TC/llvm-ar \
        RANLIB_aarch64_linux_android=$TC/llvm-ranlib \
        RANLIB_x86_64_linux_android=$TC/llvm-ranlib \
        cargo build --release --target "$target"
done
ls -l target/*/release/libmain.so

echo "== aapt2 link"
rm -f "$OUT/base.apk"
# Resources are compiled when there are any: the theme lives there.
res=()
if [ -d android/res ]; then
    "$BT/aapt2" compile --dir android/res -o "$OUT/res.zip"
    res=(-R "$OUT/res.zip")
fi
"$BT/aapt2" link \
    -o "$OUT/base.apk" \
    -I "$PLATFORM" \
    --manifest android/AndroidManifest.xml \
    ${res[@]+"${res[@]}"} \
    --auto-add-overlay \
    --min-sdk-version "$API" \
    --target-sdk-version 36 \
    --version-code "${VERSION_CODE:-2}" \
    --version-name "${VERSION_NAME:-0.2}"

echo "== add native libraries"
args=()
for abi in ${ABIS//,/ }; do
    args+=("$abi:target/$(target_of "$abi")/release/libmain.so")
done
python3 - "$OUT/base.apk" "${args[@]}" <<'PY'
import sys, zipfile

apk, libs = sys.argv[1], sys.argv[2:]
with zipfile.ZipFile(apk, "a", zipfile.ZIP_DEFLATED) as z:
    for lib in libs:
        abi, path = lib.split(":", 1)
        name = f"lib/{abi}/libmain.so"
        z.write(path, name)
        print("  ", name)
PY

echo "== java side"
bash tools/build-java.sh
python3 - "$OUT/base.apk" "$OUT/dex/classes.dex" <<'PY'
import sys, zipfile

apk, dex = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(apk, "a", zipfile.ZIP_DEFLATED) as z:
    z.write(dex, "classes.dex")
    print("   classes.dex")
PY

echo "== zipalign"
"$BT/zipalign" -f -p 4 "$OUT/base.apk" "$OUT/aligned.apk"
echo "== sign"
# The key lives OUTSIDE target/android: a `cargo clean` or a wiped target must
# not mint a new one, or the phone refuses the update ("app not installed")
# because the package would be signed by a different certificate.
KEYDIR=${KEYDIR:-keys}
mkdir -p "$KEYDIR"
KS=$KEYDIR/dev.p12
PASS=${KEYPASS:-android}
if [ ! -f "$KS" ]; then
    # keytool is not available (JRE only), so openssl makes the PKCS#12 bundle.
    openssl req -x509 -newkey rsa:2048 -keyout "$KEYDIR/key.pem" -out "$KEYDIR/cert.pem" \
        -days 10000 -nodes -subj "/CN=pdf viewer" 2>/dev/null
    openssl pkcs12 -export -inkey "$KEYDIR/key.pem" -in "$KEYDIR/cert.pem" -out "$KS" \
        -name pdfviewer -passout "pass:$PASS"
fi
JAVA_HOME=$SDK/jdk17 PATH=$SDK/jdk17/bin:$PATH "$BT/apksigner" sign \
    --ks "$KS" --ks-type PKCS12 --ks-pass "pass:$PASS" \
    --ks-key-alias pdfviewer \
    --out "$OUT/pdf-viewer.apk" "$OUT/aligned.apk"

echo "== done"
ls -l "$OUT/pdf-viewer.apk"
echo "   signed with $KS"
echo "   sha256 $(openssl pkcs12 -in "$KS" -nokeys -passin "pass:$PASS" 2>/dev/null | openssl x509 -noout -fingerprint -sha256 | cut -d= -f2)"
