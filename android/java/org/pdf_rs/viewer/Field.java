package org.pdf_rs.viewer;

import android.app.Activity;
import android.content.Context;
import android.text.Editable;
import android.text.TextWatcher;
import android.util.Log;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.KeyEvent;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputMethodManager;
import android.widget.EditText;
import android.widget.FrameLayout;
import android.widget.PopupWindow;
import android.widget.TextView;

/**
 * The viewer's text fields, as framework widgets.
 *
 * <p>winit's Android backend has no input-method support: it can ask for the
 * soft keyboard and it gets key events back, which is enough for a hand-drawn
 * keyboard but not for an input method. Composing and autocorrect, swipe
 * typing, emoji, dictation, clipboard paste, cursor keys and selection handles
 * all go through an {@code InputConnection}, and an {@code InputConnection}
 * only exists for a view that is a text editor. So the address bar and the
 * search panel borrow the framework's own {@link EditText}: the app draws the
 * pill, this draws everything inside it.
 *
 * <p>Native code runs on android-activity's own thread, not on the Java main
 * thread, and a {@link View} may only be built, laid out and focused on the UI
 * thread. Every call that touches the view is therefore posted with
 * {@link Activity#runOnUiThread}; what native code needs back - the text and
 * whether the keyboard's action key was pressed - is mirrored into volatile
 * fields that can be read from any thread.
 */
public final class Field {
    /** The one field, shared by the address bar and the search panel. */
    private static EditText view;
    private static FrameLayout.LayoutParams params;
    private static PopupWindow popup;
    private static InputMethodManager imm;

    private static final String TAG = "pfdField";

    /** The text, as the {@link TextWatcher} last saw it. Any thread. */
    private static volatile String text = "";

    /** Bumped whenever the owner changes the text. Any thread. */
    private static volatile int revision = 0;

    /** Bumped when the keyboard's Go / Search key is pressed. Any thread. */
    private static volatile int action = 0;

    /** Set while native code is the one writing, so it is not reported back. */
    private static boolean writing;

    /** What the view is configured for, so a frame changes nothing. */
    private static int configuredInput = -1;
    private static int configuredIme = -1;
    private static String configuredHint = null;

    private static final int TEXT_COLOUR = 0xFFECEDF4;
    private static final int HINT_COLOUR = 0xFF7E808C;

    private Field() {}

    /** The text as of the last change. Callable from any thread. */
    public static String text() {
        return text;
    }

    /** How many times the owner has changed the text. Any thread. */
    public static int revision() {
        return revision;
    }

    /** How many times the Go / Search key has been pressed. Any thread. */
    public static int action() {
        return action;
    }

    /**
     * Put the field at ({@code x}, {@code y}) with size {@code w} x {@code h},
     * all in window pixels, showing {@code value} unless it already does.
     * Callable from any thread; the work happens on the UI thread.
     */
    public static void show(final Activity activity, final int x, final int y,
                            final int w, final int h, final float px,
                            final int inputType, final int imeOptions,
                            final String hint, final String value) {
        if (activity == null) {
            return;
        }
        activity.runOnUiThread(new Runnable() {
            @Override
            public void run() {
                showOnUi(activity, x, y, w, h, px, inputType, imeOptions, hint, value);
            }
        });
    }

    /** Take the field and its keyboard down. Callable from any thread. */
    public static void hide(final Activity activity) {
        if (activity == null) {
            return;
        }
        activity.runOnUiThread(new Runnable() {
            @Override
            public void run() {
                if (popup == null) {
                    return;
                }
                if (imm != null && view != null) {
                    imm.hideSoftInputFromWindow(view.getWindowToken(), 0);
                }
                if (view != null) {
                    view.clearFocus();
                }
                if (popup.isShowing()) {
                    popup.dismiss();
                }
                // The next show has to configure the input again: it may be the
                // other field's turn.
                configuredInput = -1;
                configuredIme = -1;
                configuredHint = null;
            }
        });
    }

    private static void showOnUi(Activity activity, int x, int y, int w, int h,
                                 float px, int inputType, int imeOptions,
                                 String hint, String value) {
        if (view == null) {
            create(activity, w, h, x, y);
        }
        // `params` is the popup's own layout params in this case: it carries
        // the size the field is laid out at.
        params.width = w;
        params.height = h;

        // The pill the app draws is 40dp tall with a 12dp lead: the text sits
        // in the same place if the padding and the size match.
        int pad = (int) (12.0f * px / 15.0f);
        view.setPadding(pad, 0, Math.max(0, pad - 2), 0);
        view.setTextSize(TypedValue.COMPLEX_UNIT_PX, px);

        // Setting the input type or the hint restarts the input method, so it
        // is done when it changes and not once a frame.
        if (configuredInput != inputType || configuredIme != imeOptions) {
            view.setInputType(inputType);
            view.setImeOptions(imeOptions);
            configuredInput = inputType;
            configuredIme = imeOptions;
        }
        if (hint != null && !hint.equals(configuredHint)) {
            view.setHint(hint);
            configuredHint = hint;
        }

        // Text from native code (a URL the app was asked to open, a query it
        // restores) is not something the owner typed.
        if (value != null && !value.equals(text)) {
            writing = true;
            view.setText(value);
            view.setSelection(value.length());
            text = value;
            writing = false;
        }

        if (!popup.isShowing()) {
            try {
                popup.showAtLocation(activity.getWindow().getDecorView(),
                        Gravity.TOP | Gravity.START, x, y);
            } catch (RuntimeException e) {
                Log.w(TAG, "showAtLocation: " + e);
                return;
            }
        } else {
            popup.update(x, y, w, h);
        }
        if (!view.isFocused()) {
            view.requestFocus();
            if (imm != null) {
                imm.showSoftInput(view, InputMethodManager.SHOW_IMPLICIT);
            }
        }
    }

    private static void create(Activity activity, int w, int h, int x, int y) {
        view = new EditText(activity);
        // No framework background: the app draws the pill behind the text.
        view.setBackground(null);
        view.setIncludeFontPadding(false);
        view.setSingleLine(true);
        view.setGravity(Gravity.CENTER_VERTICAL | Gravity.START);
        view.setTextColor(TEXT_COLOUR);
        view.setHintTextColor(HINT_COLOUR);
        view.setShowSoftInputOnFocus(true);
        view.setFocusable(true);
        view.setFocusableInTouchMode(true);
        view.setCursorVisible(true);
        view.addTextChangedListener(new TextWatcher() {
            @Override
            public void beforeTextChanged(CharSequence s, int start, int count, int after) {}

            @Override
            public void onTextChanged(CharSequence s, int start, int before, int count) {}

            @Override
            public void afterTextChanged(Editable s) {
                if (writing) {
                    return;
                }
                text = s.toString();
                revision++;
            }
        });
        // The keyboard's Go / Search key is the bar's own button: an editor
        // action never reaches the app as a key event.
        view.setOnEditorActionListener(new TextView.OnEditorActionListener() {
            @Override
            public boolean onEditorAction(TextView v, int actionId, KeyEvent event) {
                if (actionId == EditorInfo.IME_ACTION_GO
                        || actionId == EditorInfo.IME_ACTION_SEARCH
                        || event == null
                        || event.getKeyCode() == KeyEvent.KEYCODE_ENTER) {
                    action++;
                    return true;
                }
                return false;
            }
        });
        imm = (InputMethodManager) activity.getSystemService(Context.INPUT_METHOD_SERVICE);
        params = new FrameLayout.LayoutParams(w, h);
        // The field lives in a window of its own, because a `NativeActivity`'s
        // window has no view surface at all: the activity hands its surface to
        // native code (`Window.takeSurface`), so a view added to the content
        // view is laid out and drawn into nothing. A popup is a window, with a
        // surface of its own, above the native one.
        popup = new PopupWindow(view, w, h, true);
        popup.setBackgroundDrawable(null);
        popup.setClippingEnabled(false);
        popup.setInputMethodMode(PopupWindow.INPUT_METHOD_NEEDED);
        // Not touchable: taps belong to the bar and the page underneath (its
        // buttons, its drags), and a touchable popup would swallow them. The
        // keyboard works through the input connection, which does not care.
        popup.setTouchable(false);
        popup.setOutsideTouchable(false);
    }
}
