package org.pdf_rs.viewer;

import android.app.NativeActivity;
import android.content.Intent;

/**
 * The activity the manifest names: a {@link NativeActivity} with two fixes.
 *
 * <p>With {@code launchMode="singleTask"} a second VIEW intent arrives as
 * {@code onNewIntent}, and Android does <em>not</em> update {@code getIntent()}
 * by itself — an activity has to call {@code setIntent()} for that. So a
 * second "Open with" while the app was running left {@code getIntent()}
 * answering with the first document, and the native side, which reads the URI
 * back out of the activity through JNI, never saw the new one: the file picker
 * appeared to do nothing and the old page stayed on screen.
 *
 * <p>{@code NativeActivity.onNewIntent} is protected, so this is the one place
 * that can do the swap before the native code looks.
 *
 * <p>And the native event loop knows nothing about this: Android brings a
 * `singleTask` activity forward without changing the window's focus, so no
 * window event fires. {@link #nativeNewIntent()} is the poke — it raises a
 * flag and asks for a frame, and the frame that follows reads the new intent.
 */
public class Viewer extends NativeActivity {
    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        try {
            nativeNewIntent();
        } catch (UnsatisfiedLinkError e) {
            // No native side (a build without the library): nothing to tell.
        }
    }

    /** Implemented in Rust (`intent.rs`), mangled as
     *  {@code Java_org_pdf_1rs_viewer_Viewer_nativeNewIntent}. */
    private static native void nativeNewIntent();
}
