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

* `pdf` — `https://github.com/s3bk/pdf.git`, branch `master`. Not on crates.io
  in this form: the viewer builds the fork. It is spelled exactly as
  `pdf_render` spells it, on purpose: two source entries for one crate is two
  `pdf` crates in the tree and two incompatible `Page` types. `Cargo.lock` holds
  the revision the release was built with (`e7bda78`).
* `pdf_render` — `https://github.com/s3bk/pdf_render.git`, pinned to
  `075ea26` (branch `vello`). A path into a sibling checkout is no use to
  anyone who clones this repository, and a tag that only builds on one machine
  is not a release.
* Both are read over ssh, which is what `pdf_render`'s own manifest does. With a
  GitHub key that just works; without one, the two URLs have to become
  `https://` in both manifests.
* `font` — `pdf-rs/font`, pinned by revision. The same parser `pdf` uses, so the
  UI borrows glyph outlines rather than pulling in a second text stack.

## Signing

`keys/dev.p12` is the throwaway development key (`pass:android`, alias
`pdfviewer`, `CN=pdf viewer`). It is committed on purpose: an update only
installs over the previous build if it is signed with the same certificate, and
the last builds on the emulator used this one. It is not a secret and it is not
what a published app would use.

## Releases

A tagged release carries the signed APK for each ABI:

    v0.9.0   pdf-viewer-0.9.0-arm64-v8a.apk    phones
             pdf-viewer-0.9.0-x86_64.apk       emulator, x86 tablets

    adb install -r pdf-viewer-0.9.0-arm64-v8a.apk

Every build is signed with `keys/dev.p12`, so a release installs over an older
one instead of demanding an uninstall. `CHANGELOG.md` says what changed.

## Status

0.9.0 (versionCode 19), tagged `v0.9.0`. Known issues live in the log page and
in the `pdf` / `pdf_render` forks.
