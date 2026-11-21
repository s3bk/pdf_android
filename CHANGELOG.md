# Changelog

## v0.9.0 — 2026-11-21

The first tagged release. The viewer has been in use from the emulator and from
`dev.qdat.net` for a while; this is the same build, pinned so it can be pointed
at.

**Viewing**

* A `NativeActivity` with a Rust cdylib: no Java, no gradle, no Android Studio.
  winit owns the window and input, wgpu draws through Vulkan, vello rasterises,
  `pdf_render` turns a page into a vello `Scene`.
* Opens what the launcher hands it: a URL, a file path, or a `content://` URI
  from a document provider. A second `ACTION_VIEW` on a running instance
  replaces the document (`onNewIntent`).
* Swipe for the next/previous page, drag and pinch to pan and zoom. The page is
  laid out for the whole window; the title strip and the bottom bar are overlays,
  so showing them never moves the page.
* Pages render in the background and are cached by range; while a new raster is
  not ready the last good frame stays on screen, scaled.

**Finding things**

* The document outline (`src/outline.rs`) and find-in-page (`src/search.rs`).
* A bottom bar with the page number, the chapter, and a slider.
* The debug/log page (`src/logview.rs`): what went wrong with the file that is
  open — missing fonts, damaged pages, unreachable URLs.

**Documents**

* Remote documents over TLS (`src/net.rs`, `src/remote.rs`) — rustls, because
  the NDK has no TLS stack and there is no Java to borrow one from. The on-screen
  URL field (`src/field.rs`) takes an address directly.

**Building**

* `build.sh` does it without gradle: cargo for the `.so`, aapt2 for the manifest,
  d8 for the one Java class, zipalign and apksigner for the package.
* `mkrelease.py <version> <code> <abi>` builds, signs, verifies the signature and
  names the APK.
* Release profile: fat LTO, one codegen unit, symbols stripped, `panic = "abort"`,
  NEON asked for by name on arm64. `opt-level = "z"` was tried and rejected — it
  cost half the frame rate.

**Known issues** live in the log page and in the `pdf` / `pdf_render` forks.
