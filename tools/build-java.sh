#!/bin/bash
# Compile the Java side - one class - and dex it.
#
# The SDK ships a JRE only (no javac), so the compile is done by ECJ, a Java
# compiler in a jar, and the dex by d8 out of build-tools. Nothing here needs
# gradle, and nothing here needs a JDK.
set -euo pipefail
cd "$(dirname "$0")/.."

SDK=${ANDROID_SDK:-/home/hematite/android-sdk}
BT=$SDK/build-tools/36.0.0
PLATFORM=$SDK/platforms/android-36/android.jar
JAVA=$SDK/jdk17/bin/java
OUT=target/android
SRC=android/java
JOUT=$OUT/javac

rm -rf "$JOUT" "$OUT/dex"
mkdir -p "$JOUT" "$OUT/dex"

echo "== ecj"
find "$SRC" -name '*.java' -print0 | xargs -0 "$JAVA" -jar tools/ecj.jar \
    -source 8 -target 8 \
    -classpath "$PLATFORM" \
    -nowarn \
    -d "$JOUT"

echo "== d8"
JAVA_HOME=$SDK/jdk17 PATH=$SDK/jdk17/bin:$PATH "$BT/d8" \
    --lib "$PLATFORM" --min-api 26 --output "$OUT/dex" \
    $(find "$JOUT" -name '*.class')

ls -l "$OUT/dex/classes.dex"
