# pdf-android

A native Android PDF viewer: `NativeActivity` + a cdylib, no Java and no
gradle. winit owns the window and the input, wgpu draws through Vulkan, vello
rasterises, and `pdf_render` turns a page into a vello `Scene`.

Package `org.pdf_rs.viewer`, min SDK 26, one activity (`android.app.NativeActivity`).

## What it does

* Opens a PDF from the intent the launcher handed us (see `src/intent.rs`): a
  URL, a file path, or a `content://` URI out of a document provider. Opening a
  second document from the picker replaces the first (`ACTION_VIEW` on a running
  instance is picked up through `onNewIntent`).
* Swipe horizontally for the next/previous page, drag and pinch to pan and zoom;
  the page is laid out for the whole window, and the top strip / bottom bar are
  overlays over it, so showing or hiding them never moves the page.
* Outlines (`src/outline.rs`), text search (`src/search.rs`), a text field for
  URLs (`src/field.rs`), remote documents over TLS (`src/remote.rs`, `src/net.rs`),
  and a debug/log page (`src/logview.rs`) that records what went wrong with the
  file that is open — missing fonts and the like.
* Pages are rendered in the background and cached; while a new raster is not
  ready the last good frame stays on screen, scaled (`pdf_render`'s range cache).

## Layout

    src/lib.rs        the app: window, input, page layout, scene, present
    src/intent.rs     the launch/new intent, ContentResolver, file opening
    src/topbar.rs     the title strip and the menu
    src/nav.rs        the bottom bar: page number, chapter, slider
    src/field.rs      the on-screen text field (URLs, search terms)
    src/outline.rs    the outline tree
    src/search.rs     find in page
    src/remote.rs     fetch a document over https (rustls)
    src/logview.rs    the debug/log page
    android/          AndroidManifest.xml, res/, the Java shim (if any)
    tools/            build-java.sh, ecj.jar, pngstat.py
    mkrelease.py      version bump, build, sign, verify, name the APK
    icons/            the icon and its sources (svg / typst / python)

## Build

Needs the Android SDK (`ANDROID_SDK`, default `/home/hematite/android-sdk`:
NDK r29, build-tools 36.0.0, platforms/android-36, jdk17 in the SDK) and the
Rust targets for arm64 and x86_64.

    ./build.sh                    # both ABIs into one APK
    ABI=arm64-v8a ./build.sh      # just the phone: a much smaller APK
    python3 mkrelease.py 0.9.0 19 arm64-v8a   # version, versionCode, ABIs

The result lands in `target/android/`; `mkrelease.py` also verifies the
signature and prints the SHA-256.

Install on a device or emulator:

    adb install -r target/android/pdf-viewer-<version>-arm64-v8a.apk
    adb exec-out sh -c 'pm install -r /data/local/tmp/pdf-viewer.apk'   # if adb install fails

## Dependencies (pay attention to these)

* `pdf` — `ssh://git@github.com/s3bk/pdf.git`, branch `master`. Not on crates.io
  in this form: the app builds the fork.
* `pdf_render` — a **local path**, `/home/hematite/task-clones/pdf_render/render`,
  branch `vello` (pushed to `s3bk/pdf_render` as `vello`). Adjust the path in
  `Cargo.toml` to build somewhere else.
* `Cargo.toml` still carries a `[patch]` that repoints `pdf` at a cargo git
  checkout; it is there so edits to the checkout are picked up, and the two
  tolerance fixes its comment describes are **not** in `pdf` master (the
  comment is stale — see the log page for what actually fails on a file).

## Signing

`keys/dev.p12` is the throwaway development key (`pass:android`, alias
`pdfviewer`, `CN=pdf viewer`). It is committed on purpose: an update only
installs over the previous build if it is signed with the same certificate, and
the last builds on the emulator used this one. It is not a secret and it is not
what a published app would use.

## Status

0.9.0 (versionCode 19). Known issues live in the log page and in the
`pdf`/`pdf_render` forks.
