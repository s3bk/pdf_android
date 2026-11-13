//! The two text fields, as the framework's own widgets.
//!
//! winit's Android backend has no IME support: `set_ime_allowed` only asks the
//! system keyboard to come up, and what the keyboard sends back are
//! `InputQueue` key events, which arrive as `WindowEvent::KeyboardInput`. That
//! is enough for a hand-drawn keyboard, and it is all a `NativeActivity` gets
//! without a real editor view. It is not an input method: composing and
//! autocorrect, swipe typing, emoji, dictation, clipboard paste, cursor keys
//! and selection handles all go through an `InputConnection`, and an
//! `InputConnection` only exists for a view that is a text editor.
//!
//! So the fields are real `EditText`s. They are built and driven by
//! [`org.pdf_rs.viewer.Field`] - the APK's one Java class, about a page of it -
//! because a `View` may only be touched on the Java UI thread and
//! android-activity runs the event loop on a thread of its own. That class lays
//! the field exactly over the pill [`crate::url`] and [`crate::search`] paint,
//! gives it no background of its own (the pill and the bar behind it stay
//! ours), and mirrors the text and the keyboard's Go / Search key into fields
//! that can be read from this thread.
//!
//! This side is a thin client: it says where the field goes and what it holds,
//! and it polls the two counters once a frame.

/// What a field is for: the address bar, or the search query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    Url,
    Query,
}

#[cfg(target_os = "android")]
pub use android::Field;

#[cfg(target_os = "android")]
mod android {
    use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
    use jni::sys::jint;
    use vello::kurbo::Rect;

    use super::Purpose;
    use crate::intent;

    /// `InputType.TYPE_CLASS_TEXT | TYPE_TEXT_VARIATION_URI`, and the plain
    /// `TYPE_CLASS_TEXT` for a query.
    const TYPE_URL: jint = 0x0001 | 0x0010;
    const TYPE_QUERY: jint = 0x0001;
    /// `EditorInfo.IME_ACTION_GO` / `IME_ACTION_SEARCH`, each with
    /// `IME_FLAG_NO_FULLSCREEN`: in landscape a full-screen extract view would
    /// cover the bar the field belongs to.
    const IME_GO: jint = 0x0002 | 0x1000_0000;
    const IME_SEARCH: jint = 0x0003 | 0x1000_0000;

    const HINT_URL: &str = "https://example.com/file.pdf";
    const HINT_QUERY: &str = "Search the document";

    /// The Java class that owns the `EditText`, as a global reference: it lives
    /// as long as the process does.
    pub struct Field {
        class: GlobalRef,
        /// The text-change and action counters as last read.
        revision: jint,
        action: jint,
        /// What is in the field, as far as both sides agree.
        text: String,
        /// What was last read back from the field, or `None` while it is down.
        /// Pushing a value that equals this would be writing back a frame that
        /// is behind, and would eat the character that just arrived.
        received: Option<String>,
        /// Which field the Java side is currently configured for.
        purpose: Option<Purpose>,
        /// Whether the field is up, so `hide` is not posted every frame.
        shown: bool,
        /// What the field was last told, so a still frame says nothing.
        last: Option<(Purpose, i64, i64, i64, i64, i64, bool)>,
    }

    impl Field {
        /// Find the Java class, through the activity's own class loader: this
        /// thread was attached to the VM by us, so a plain `FindClass` would
        /// look in the boot class loader and never see the APK.
        pub fn new() -> Option<Field> {
            intent::with_env(|env| {
                let activity = intent::activity()?;
                let loader = env
                    .call_method(&activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
                    .ok()?
                    .l()
                    .ok()?;
                let name = env.new_string("org.pdf_rs.viewer.Field").ok()?;
                let loaded = env.call_method(
                    &loader,
                    "loadClass",
                    "(Ljava/lang/String;)Ljava/lang/Class;",
                    &[JValue::Object(&JObject::from(name))],
                );
                let class = match loaded {
                    Ok(value) => value.l().ok()?,
                    Err(e) => {
                        // A missing class leaves a pending exception; it is
                        // reported once and cleared, not left to poison the
                        // next JNI call on this thread.
                        log::warn!("field: loadClass: {e}");
                        if env.exception_check().unwrap_or(false) {
                            env.exception_describe().ok();
                            env.exception_clear().ok();
                        }
                        return None;
                    }
                };
                if class.is_null() {
                    log::warn!("field: org.pdf_rs.viewer.Field not found");
                    return None;
                }
                Some(Field {
                    class: env.new_global_ref(class).ok()?,
                    revision: 0,
                    action: 0,
                    text: String::new(),
                    received: None,
                    shown: false,
                    purpose: None,
                    last: None,
                })
            })
        }

        /// Put the field over `rect` for `purpose`. `text` is written into the
        /// field only when it is not what the field already holds - which is
        /// what keeps a poll that is one frame behind from writing back over a
        /// character that was just typed.
        pub fn show(&mut self, purpose: Purpose, rect: Rect, text: &str, px: f64) {
            // What the field holds is the truth while it is up; the bar's copy
            // is only pushed when it differs from what has actually been read
            // back.
            let push = match &self.received {
                Some(seen) => seen != text,
                None => true,
            };
            let fresh = self.purpose != Some(purpose);
            let state = (purpose, rect.x0.round() as i64, rect.y0.round() as i64,
                         rect.width().round() as i64, rect.height().round() as i64,
                         px.round() as i64, push);
            if !fresh && self.last == Some(state) {
                return;
            }
            self.last = Some(state);
            let (input, ime, hint) = match purpose {
                Purpose::Url => (TYPE_URL, IME_GO, HINT_URL),
                Purpose::Query => (TYPE_QUERY, IME_SEARCH, HINT_QUERY),
            };
            let (x, y) = (rect.x0.round() as jint, rect.y0.round() as jint);
            let w = rect.width().max(1.0).round() as jint;
            let h = rect.height().max(1.0).round() as jint;
            let Some(activity) = intent::activity() else {
                return;
            };
            log::debug!("field: show at {x},{y} {w}x{h}, px {px}, text {text:?} push {push}");
            let done = intent::with_env(|env| {
                // The hint and the text only when they change: each is a Java
                // string, and neither belongs in a frame's allocation.
                let hint = if fresh {
                    JObject::from(env.new_string(hint).ok()?)
                } else {
                    JObject::null()
                };
                let value = if push {
                    JObject::from(env.new_string(text).ok()?)
                } else {
                    JObject::null()
                };
                let class = self.class_obj();
                env.call_static_method(
                    &class,
                    "show",
                    "(Landroid/app/Activity;IIIIFIILjava/lang/String;Ljava/lang/String;)V",
                    &[
                        JValue::Object(&activity),
                        JValue::Int(x),
                        JValue::Int(y),
                        JValue::Int(w),
                        JValue::Int(h),
                        JValue::Float(px as f32),
                        JValue::Int(input),
                        JValue::Int(ime),
                        JValue::Object(&hint),
                        JValue::Object(&value),
                    ],
                )
                .ok()?;
                Some(())
            });
            if done.is_some() {
                self.shown = true;
                self.purpose = Some(purpose);
                if push {
                    self.received = Some(text.to_string());
                }
            }
        }

        /// Take the field and the keyboard down.
        pub fn hide(&mut self) {
            self.last = None;
            self.purpose = None;
            // Whatever is in the field is the bar's business; the next show
            // passes it in again.
            self.received = None;
            if !self.shown {
                return;
            }
            self.shown = false;
            let Some(activity) = intent::activity() else {
                return;
            };
            intent::with_env(|env| {
                let class = self.class_obj();
                env.call_static_method(
                    &class,
                    "hide",
                    "(Landroid/app/Activity;)V",
                    &[JValue::Object(&activity)],
                )
                .ok()?;
                Some(())
            });
            self.text.clear();
        }

        /// What the owner has typed, if it changed since the last call.
        pub fn poll(&mut self) -> Option<String> {
            let revision = self.counter("revision")?;
            if revision == self.revision {
                return None;
            }
            self.revision = revision;
            let current = intent::with_env(|env| {
                let class = self.class_obj();
                let s = env
                    .call_static_method(&class, "text", "()Ljava/lang/String;", &[])
                    .ok()?
                    .l()
                    .ok()?;
                if s.is_null() {
                    return Some(String::new());
                }
                let s = JString::from(s);
                env.get_string(&s).ok().map(String::from)
            })?;
            self.text = current.clone();
            self.received = Some(current.clone());
            log::debug!("field: text {:?}", current);
            Some(current)
        }

        /// Whether the keyboard's Go / Search key was pressed since the last
        /// call.
        pub fn took_action(&mut self) -> bool {
            let Some(action) = self.counter("action") else {
                return false;
            };
            if action == self.action {
                return false;
            }
            self.action = action;
            true
        }

        /// What the field holds, as of the last [`Field::poll`].
        #[allow(dead_code)]
        pub fn text(&self) -> &str {
            &self.text
        }

        /// One of the Java class's counters.
        fn counter(&self, what: &str) -> Option<jint> {
            intent::with_env(|env| {
                let class = self.class_obj();
                env.call_static_method(&class, what, "()I", &[])
                    .ok()?
                    .i()
                    .ok()
            })
        }

        /// The class global reference as a borrowed `JClass`, for the length of
        /// one call.
        fn class_obj(&self) -> JClass<'static> {
            unsafe { JClass::from_raw(self.class.as_obj().as_raw()) }
        }
    }
}

#[cfg(not(target_os = "android"))]
pub use desktop::Field;

#[cfg(not(target_os = "android"))]
mod desktop {
    use vello::kurbo::Rect;

    use super::Purpose;

    /// There is no framework to borrow a text field from off Android. The bar
    /// and the panel then draw their own text out of the key events winit
    /// delivers.
    pub struct Field;

    impl Field {
        pub fn new() -> Option<Field> {
            None
        }

        pub fn show(&mut self, _purpose: Purpose, _rect: Rect, _text: &str, _px: f64) {}

        pub fn hide(&mut self) {}

        pub fn poll(&mut self) -> Option<String> {
            None
        }

        pub fn took_action(&mut self) -> bool {
            false
        }

        pub fn text(&self) -> &str {
            ""
        }
    }
}
