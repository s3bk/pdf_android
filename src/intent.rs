//! What the launcher asked us to open.
//!
//! A `VIEW` intent reaches a `NativeActivity` only as far as the Java side:
//! `getIntent`, `onNewIntent` and `getData` are Java methods, and this APK has
//! no Java classes in it (`hasCode="false"`, no JNI glue of our own). So the
//! URI is read back out of the activity through JNI - once at startup, and
//! again whenever Android brings the activity back to the front, because with
//! `launchMode="singleTask"` a second `VIEW` intent arrives as `onNewIntent`
//! and a `NativeActivity` does not forward that to native code.
//!
//! Three shapes arrive, and each has its own way in:
//!
//!   * `https://host/x.pdf` - handed to the address bar's own path, a byte
//!     range at a time, so nothing is downloaded first;
//!   * `file:///path/x.pdf` - a plain path;
//!   * `content://provider/doc/…` - a document provider (Files, Downloads, a
//!     mail or chat app). There is no path in it: the bytes come out of the
//!     `ContentResolver` through an `InputStream`, into the app's own files
//!     directory, which is the one place the app may always read.
#![cfg(target_os = "android")]

use std::ffi::{c_void, CString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::Mutex;

use jni::objects::{JObject, JString};
use jni::sys::jobject;
use jni::JavaVM;
use winit::platform::android::activity::AndroidApp;

/// The `JavaVM` and the activity, both as global JNI references owned by
/// `android-activity`: they live as long as the process does, so keeping the
/// raw pointers is safe and is the only way in from a worker thread.
static VM: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ACTIVITY: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// A second `VIEW` intent arrived while the app was already in front.
///
/// `org.pdf_rs.viewer.Viewer.onNewIntent` sets the intent and calls the native
/// method at the bottom of this file, which raises this flag and wakes the
/// event loop. Watching for the window's focus is not enough: Android brings a
/// `singleTask` activity forward without taking the focus away from it, so a
/// second document opened from Files or a browser produced no focus event at
/// all and the old page stayed on screen.
static NEW_INTENT: AtomicBool = AtomicBool::new(false);

/// Asks the event loop for a frame, so it can notice [`NEW_INTENT`].
static WAKE: Mutex<Option<Box<dyn Fn() + Send + Sync>>> = Mutex::new(None);

pub fn set_wake(f: Box<dyn Fn() + Send + Sync>) {
    *WAKE.lock().unwrap() = Some(f);
}

fn wake() {
    let f = WAKE.lock().unwrap();
    if let Some(f) = f.as_ref() {
        f();
    }
}

/// Whether an intent has arrived that has not been read yet. Clears the flag.
pub fn arrived() -> bool {
    NEW_INTENT.swap(false, Ordering::SeqCst)
}

/// `Viewer.nativeNewIntent()`: called from Java's `onNewIntent`, on the
/// activity's (UI) thread.
///
/// The name is the JNI mangling of `org.pdf_rs.viewer.Viewer`: the `_` in
/// `pdf_rs` becomes `_1`.
#[no_mangle]
pub extern "system" fn Java_org_pdf_1rs_viewer_Viewer_nativeNewIntent(
    _env: *mut jni::sys::JNIEnv,
    _class: jni::sys::jclass,
) {
    NEW_INTENT.store(true, Ordering::SeqCst);
    wake();
}

/// The URI this launch was asked to open, if any. Taken, not copied twice:
/// [`take`] hands it over and leaves nothing behind.
static URI: Mutex<Option<String>> = Mutex::new(None);

/// Remember the VM and the activity and read the intent they carry. Call once,
/// first thing in `android_main`.
pub fn capture(app: &AndroidApp) {
    VM.store(app.vm_as_ptr(), Ordering::SeqCst);
    ACTIVITY.store(app.activity_as_ptr(), Ordering::SeqCst);
    register_new_intent();
    match data_uri().or_else(stream_extra) {
        Some(uri) => {
            log::info!("intent data: {uri}");
            *URI.lock().unwrap() = Some(uri);
        }
        None => log::info!("no intent data (launched from the icon)"),
    }
}

/// The URI waiting to be opened, if the activity carries one.
pub fn take() -> Option<String> {
    URI.lock().unwrap().clone()
}

/// Bind `Viewer.nativeNewIntent` by hand.
///
/// The automatic JNI lookup by mangled name does not work here: `NativeActivity`
/// calls `System.loadLibrary` from a framework class, so the library is
/// registered with the boot class loader while our activity class lives in the
/// app's loader, and the lookup that starts from the app class never finds it
/// (the call died with `UnsatisfiedLinkError`, caught and silently ignored).
/// `RegisterNatives` on the activity's own class binds it wherever the class
/// came from.
fn register_new_intent() {
    let registered = with_env(|env| {
        let activity = activity()?;
        let class = env
            .call_method(&activity, "getClass", "()Ljava/lang/Class;", &[])
            .ok()?
            .l()
            .ok()?;
        let methods = [jni::NativeMethod {
            name: "nativeNewIntent".into(),
            sig: "()V".into(),
            fn_ptr: Java_org_pdf_1rs_viewer_Viewer_nativeNewIntent as *mut c_void,
        }];
        let class = jni::objects::JClass::from(class);
        Some(match env.register_native_methods(&class, &methods) {
            Ok(()) => true,
            Err(e) => {
                log::error!("register nativeNewIntent: {e}");
                false
            }
        })
    });
    if registered != Some(true) {
        log::warn!("register nativeNewIntent: no activity to bind it to");
    }
}

/// Re-read the activity's intent and return its URI if it is a different one
/// from `handled`. This is how a second "Open with" reaches an app that is
/// already running.
pub fn changed(handled: Option<&str>) -> Option<String> {
    if VM.load(Ordering::SeqCst).is_null() {
        return None;
    }
    let uri = data_uri().or_else(stream_extra)?;
    if handled == Some(uri.as_str()) {
        return None;
    }
    log::info!("new intent data: {uri}");
    *URI.lock().unwrap() = Some(uri.clone());
    Some(uri)
}

// ---------------------------------------------------------------------------
// JNI plumbing

pub(crate) fn activity() -> Option<JObject<'static>> {
    let p = ACTIVITY.load(Ordering::SeqCst);
    if p.is_null() {
        return None;
    }
    // A global reference: it belongs to `android-activity`, so it is never
    // deleted here and is valid on any thread.
    Some(unsafe { JObject::from_raw(p as jobject) })
}

/// Run `f` with a `JNIEnv` for this thread. The thread stays attached: a
/// detach on every call would cost more than the call, and this is called once
/// a frame while a text field is up.
pub(crate) fn with_env<R>(f: impl FnOnce(&mut jni::JNIEnv) -> Option<R>) -> Option<R> {
    let p = VM.load(Ordering::SeqCst);
    if p.is_null() {
        return None;
    }
    // `from_raw` does not take ownership of the VM and the value is not
    // dropped destructively, so constructing it per call is right.
    let vm = unsafe { JavaVM::from_raw(p as *mut jni::sys::JavaVM) }.ok()?;
    let mut env = vm.attach_current_thread_permanently().ok()?;
    f(&mut env)
}

fn string_of(env: &mut jni::JNIEnv, obj: &JObject) -> Option<String> {
    if obj.is_null() {
        return None;
    }
    let s = env
        .call_method(obj, "toString", "()Ljava/lang/String;", &[])
        .ok()?
        .l()
        .ok()?;
    if s.is_null() {
        return None;
    }
    let s = JString::from(s);
    env.get_string(&s).ok().map(String::from)
}

/// `activity.getIntent()`, or `None` when the app was started from the icon.
fn intent_of(env: &mut jni::JNIEnv) -> Option<JObject<'static>> {
    let activity = activity()?;
    let intent = env
        .call_method(&activity, "getIntent", "()Landroid/content/Intent;", &[])
        .ok()?
        .l()
        .ok()?;
    if intent.is_null() {
        return None;
    }
    // The intent is a local reference of this frame, but it is used inside the
    // same `with_env` call, so the lifetime is that of the attach.
    Some(unsafe { JObject::from_raw(intent.as_raw()) })
}

fn action_of(env: &mut jni::JNIEnv, intent: &JObject) -> Option<String> {
    let action = env
        .call_method(intent, "getAction", "()Ljava/lang/String;", &[])
        .ok()?
        .l()
        .ok()?;
    string_of(env, &action)
}

/// `intent.getData().toString()`: the URI in the intent's data slot.
fn data_uri() -> Option<String> {
    with_env(|env| {
        let intent = intent_of(env)?;
        let data = env
            .call_method(&intent, "getData", "()Landroid/net/Uri;", &[])
            .ok()?
            .l()
            .ok()?;
        string_of(env, &data)
    })
}

/// `intent.getParcelableExtra(Intent.EXTRA_STREAM)`: the share sheet's file.
fn stream_extra() -> Option<String> {
    with_env(|env| {
        let intent = intent_of(env)?;
        if action_of(env, &intent)?.as_str() != "android.intent.action.SEND" {
            return None;
        }
        let key = env.new_string("android.intent.extra.STREAM").ok()?;
        let extra = env
            .call_method(
                &intent,
                "getParcelableExtra",
                "(Ljava/lang/String;)Landroid/os/Parcelable;",
                &[(&key).into()],
            )
            .ok()?
            .l()
            .ok()?;
        string_of(env, &extra)
    })
}

/// Copy a `content://` document into `dest`. Returns the number of bytes.
pub fn read_content(uri: &str, dest: &Path) -> Result<u64, String> {
    with_env(|env| Some(read_stream(env, uri, dest))).unwrap_or_else(|| Err("no JVM".into()))
}

fn read_stream(env: &mut jni::JNIEnv, uri: &str, dest: &Path) -> Result<u64, String> {
    let fail = |e: jni::errors::Error| format!("{uri}: {e}");
    let juri = JString::from(env.new_string(uri).map_err(fail)?);
    let uri_cls = env
        .find_class("android/net/Uri")
        .map_err(|e| format!("android.net.Uri: {e}"))?;
    let parsed = env
        .call_static_method(
            &uri_cls,
            "parse",
            "(Ljava/lang/String;)Landroid/net/Uri;",
            &[(&juri).into()],
        )
        .map_err(fail)?
        .l()
        .map_err(fail)?;
    let activity = activity().ok_or("no activity")?;
    let resolver = env
        .call_method(
            &activity,
            "getContentResolver",
            "()Landroid/content/ContentResolver;",
            &[],
        )
        .map_err(fail)?
        .l()
        .map_err(fail)?;
    let stream = env
        .call_method(
            &resolver,
            "openInputStream",
            "(Landroid/net/Uri;)Ljava/io/InputStream;",
            &[(&parsed).into()],
        )
        .map_err(fail)?
        .l()
        .map_err(fail)?;
    if stream.is_null() {
        return Err(format!("{uri}: the provider opened no stream"));
    }
    let mut file =
        std::fs::File::create(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let buf = env.new_byte_array(64 * 1024).map_err(fail)?;
    let mut total = 0u64;
    loop {
        let n = env
            .call_method(&stream, "read", "([B)I", &[(&buf).into()])
            .map_err(fail)?
            .i()
            .map_err(fail)?;
        if n <= 0 {
            break;
        }
        let mut bytes = env.convert_byte_array(&buf).map_err(fail)?;
        bytes.truncate(n as usize);
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        total += n as u64;
    }
    let _ = env.call_method(&stream, "close", "()V", &[]);
    if total == 0 {
        return Err(format!("{uri}: the provider sent no bytes"));
    }
    Ok(total)
}

// ---------------------------------------------------------------------------
// URIs into paths

/// The path inside a `file://` URI, with `%XX` escapes undone.
pub fn file_path(uri: &str) -> PathBuf {
    let rest = uri.strip_prefix("file://").unwrap_or(uri);
    // `file:///sdcard/x` leaves `/sdcard/x`; `file://localhost/sdcard/x` leaves
    // `localhost/sdcard/x`, so a leading host component is dropped too.
    let rest = rest.strip_prefix("localhost/").map(|r| format!("/{r}")).unwrap_or_else(|| rest.to_string());
    PathBuf::from(percent_decode(&rest))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The name the document provider itself gives `uri`, if it has one.
///
/// A `content://` URI carries no name of its own:
/// `content://media/external/file/1000000390` ends in a row id, so naming the
/// copy after the URI left "1000000390" in the top strip - the reader's own
/// file, called what the provider calls it (`OpenableColumns.DISPLAY_NAME`).
pub fn display_name(uri: &str) -> Option<String> {
    with_env(|env| display_name_of(env, uri))
}

fn display_name_of(env: &mut jni::JNIEnv, uri: &str) -> Option<String> {
    let name = display_name_in(env, uri);
    // Whether a name came back or not, a Java exception thrown on the way (a
    // provider that refuses the query) is cleared here: it was raised inside
    // this frame, and a pending one would be inherited by the next JNI call -
    // the document itself would then fail to open for want of a name.
    if let Ok(true) = env.exception_check() {
        log::warn!("{uri}: the provider threw while being asked for a name");
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    if let Some(ref n) = name {
        log::info!("{uri}: the provider calls it {n}");
    }
    name
}

fn display_name_in(env: &mut jni::JNIEnv, uri: &str) -> Option<String> {
    let juri = JString::from(env.new_string(uri).ok()?);
    let uri_cls = env.find_class("android/net/Uri").ok()?;
    let parsed = env
        .call_static_method(
            &uri_cls,
            "parse",
            "(Ljava/lang/String;)Landroid/net/Uri;",
            &[(&juri).into()],
        )
        .ok()?
        .l()
        .ok()?;
    let activity = activity()?;
    let resolver = env
        .call_method(
            &activity,
            "getContentResolver",
            "()Landroid/content/ContentResolver;",
            &[],
        )
        .ok()?
        .l()
        .ok()?;
    // A null projection returns every column, which is what looking up the
    // index needs; asking for one column would mean building a `String[]`.
    let null = JObject::null();
    let cursor = env
        .call_method(
            &resolver,
            "query",
            "(Landroid/net/Uri;[Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;Ljava/lang/String;)Landroid/database/Cursor;",
            &[
                (&parsed).into(),
                (&null).into(),
                (&null).into(),
                (&null).into(),
                (&null).into(),
            ],
        )
        .ok()?
        .l()
        .ok()?;
    if cursor.is_null() {
        log::warn!("{uri}: the provider's query returned no cursor");
        return None;
    }
    let name = (|| {
        let moved = env
            .call_method(&cursor, "moveToFirst", "()Z", &[])
            .ok()?
            .z()
            .ok()?;
        if !moved {
            log::warn!("{uri}: the provider's query returned no row");
            return None;
        }
        let column = JString::from(env.new_string("_display_name").ok()?);
        let idx = env
            .call_method(
                &cursor,
                "getColumnIndex",
                "(Ljava/lang/String;)I",
                &[(&column).into()],
            )
            .ok()?
            .i()
            .ok()?;
        if idx < 0 {
            log::warn!("{uri}: the provider's cursor has no _display_name");
            return None;
        }
        let value = env
            .call_method(
                &cursor,
                "getString",
                "(I)Ljava/lang/String;",
                &[jni::objects::JValue::Int(idx)],
            )
            .ok()?
            .l()
            .ok()?;
        string_of(env, &value)
    })();
    let _ = env.call_method(&cursor, "close", "()V", &[]);
    name
}

/// A name that is safe as one path component, for the copy of a document
/// provider's file: the last path segment of the URI, minus anything odd.
pub fn safe_name(uri: &str) -> String {
    let tail = uri.rsplit('/').next().unwrap_or("document");
    let tail = tail.split(['?', '#']).next().unwrap_or(tail);
    component(tail)
}

/// `name` as one safe path component, ending in `.pdf`.
pub fn component(name: &str) -> String {
    let mut name: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if name.is_empty() || name.starts_with('.') {
        name = format!("document{name}");
    }
    if !name.to_ascii_lowercase().ends_with(".pdf") {
        name.push_str(".pdf");
    }
    name
}

/// Kept so the compiler does not warn about the C string import when the
/// module is built without the tests that use it.
#[allow(dead_code)]
fn _cstr(s: &str) -> Option<CString> {
    CString::new(s).ok()
}
