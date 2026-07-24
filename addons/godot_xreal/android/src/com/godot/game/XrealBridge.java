package com.godot.game;

import android.app.Activity;
import android.app.ActivityManager;
import android.app.ActivityOptions;
import android.app.PictureInPictureParams;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.hardware.display.DisplayManager;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;
import android.util.Rational;
import android.view.Display;
import android.util.DisplayMetrics;
import java.lang.ref.WeakReference;

/**
 * Bridges the host {@link Activity} to the godot-xreal GDExtension's native code.
 *
 * Godot does not populate the Rust {@code ndk-context} crate's process-global context, so
 * the XREAL session bootstrap (which needs the Activity as the Unity SDK's
 * {@code unityActivity}) has no way to find it. This class hands the Activity to the
 * native side, which publishes it into {@code ndk-context}.
 *
 * Call {@link #register(Activity)} once, early (from {@code GodotApp.onCreate}); the native
 * side is idempotent. This file is part of the custom Android build template — re-apply it
 * if the template is reinstalled.
 */
public final class XrealBridge {
	private static final String TAG = "xreal";
	private static final String BRIDGE_VERSION = "display-routing-v3";
	private static boolean nativeLibrariesLoaded = false;
	private static boolean companionLaunchRequested = false;
	private static boolean displayListenerRegistered = false;
	/// The most recently registered Activity, held weakly so the process-static DisplayListener
	/// (registered once, never unregistered) does not pin an old Activity across recreation and
	/// does not call runOnUiThread/startActivity on a destroyed one. Updated by every register().
	private static WeakReference<Activity> currentActivityRef = new WeakReference<>(null);
	/// Display id of the XREAL glasses while connected (-1 = none); used to recognise its removal,
	/// since onDisplayRemoved cannot query the (already gone) Display.
	private static int xrealDisplayId = -1;

	private XrealBridge() {}

	/**
	 * Find the connected XREAL glasses display, or null when none is present.
	 *
	 * Only a display {@link #isXrealDisplay} positively identifies is returned. There is deliberately
	 * no "first non-default display" fallback: callers treat the result as the glasses
	 * (notifyGlassesConnected + companion Activity launch), so an external monitor, a Chromecast/
	 * screen-cast display or any virtual display would otherwise be mistaken for them.
	 *
	 * Every non-default display that was rejected is logged, so a device log shows immediately if the
	 * real glasses fail the match (e.g. a name or resolution we do not know about yet).
	 */
	static Display findXrealDisplay(Context context) {
		DisplayManager displayManager = (DisplayManager) context.getSystemService(Context.DISPLAY_SERVICE);
		if (displayManager == null) {
			return null;
		}

		Display[] displays = displayManager.getDisplays();
		StringBuilder rejected = null;
		for (Display display : displays) {
			if (display.getDisplayId() == Display.DEFAULT_DISPLAY) {
				continue;
			}
			if (isXrealDisplay(display)) {
				return display;
			}
			if (rejected == null) {
				rejected = new StringBuilder();
			} else {
				rejected.append("; ");
			}
			rejected.append(describeDisplay(display));
		}
		if (rejected != null) {
			Log.w(TAG, BRIDGE_VERSION + ": non-default display(s) present but NOT recognised as XREAL: "
					+ rejected
					+ " -- match needs the name to contain xreal/nreal, or a 3840x1080 real size; "
					+ "treating as no glasses (no connect notification, no companion Activity)");
		}
		return null;
	}

	static boolean isXrealDisplay(Display display) {
		if (display == null || display.getDisplayId() == Display.DEFAULT_DISPLAY) {
			return false;
		}
		String name = display.getName() == null ? "" : display.getName().toLowerCase();
		DisplayMetrics metrics = new DisplayMetrics();
		display.getRealMetrics(metrics);
		boolean xrealName = name.contains("xreal") || name.contains("nreal");
		boolean xrealLikeResolution = metrics.widthPixels == 3840 && metrics.heightPixels == 1080;
		return xrealName || xrealLikeResolution;
	}

	static String describeDisplay(Display display) {
		DisplayMetrics metrics = new DisplayMetrics();
		display.getRealMetrics(metrics);
		return display.getDisplayId() + " (" + display.getName() + ", "
				+ metrics.widthPixels + "x" + metrics.heightPixels + ")";
	}

	private static synchronized void ensureNativeLibrariesLoaded() {
		if (nativeLibrariesLoaded) {
			return;
		}
		// The XREAL native libraries must be loaded via System.loadLibrary (not just
		// dlopen'd from Rust) so the Android runtime invokes each one's JNI_OnLoad with the
		// real JavaVM. Unity does this implicitly for every Plugins/Android .so; we are not
		// Unity, so without it their JavaVM globals stay null and CreateSession crashes
		// (NativeAPI::Create -> libnr_loader.so JNI_OnLoad with a null vm). Order: lower
		// loaders first, then the XREAL wrappers, then our GDExtension.
		//
		// The nr_* wrappers and our GDExtension are REQUIRED: if any fails, CreateSession would later
		// crash with a confusing null-vm error, so we leave nativeLibrariesLoaded false and let a later
		// register() retry. `&=` (not `&&`) so every library is still attempted and its own failure is
		// logged, rather than short-circuiting at the first miss.
		boolean allRequiredLoaded = true;
		allRequiredLoaded &= loadNative("nr_loader");
		allRequiredLoaded &= loadNative("nr_api");
		allRequiredLoaded &= loadNative("XREALNativeSessionManager");
		allRequiredLoaded &= loadNative("XREALXRPlugin");
		// media_codec (FPV HW encoder) must go through System.loadLibrary too: its JNI_OnLoad, run
		// with the real JavaVM, creates a global manager singleton the encoder dereferences. Merely
		// dlopen'ing it from Rust skips JNI_OnLoad, leaving that singleton null → HWEncoderStart /
		// HWEncoderSetMediaProjection crash (SIGSEGV, null+0x38). This one is OPTIONAL: only the FPV
		// streaming feature needs it, so a load failure must not block the core XREAL session.
		loadNative("media_codec");
		allRequiredLoaded &= loadNative("godot_xreal");
		if (allRequiredLoaded) {
			nativeLibrariesLoaded = true;
		} else {
			Log.e(TAG, "one or more required XREAL native libraries failed to load; "
					+ "leaving native init incomplete so a later register() can retry");
		}
	}

	/** Load one native library. Returns whether it loaded (logging the library name on failure). */
	private static boolean loadNative(String name) {
		try {
			System.loadLibrary(name);
			Log.i(TAG, "loaded lib" + name + ".so");
			return true;
		} catch (Throwable t) {
			Log.e(TAG, "Unable to load lib" + name + ".so", t);
			return false;
		}
	}

	/** Publish the Activity to native code (safe to call more than once). */
	public static void register(Activity activity) {
		if (activity == null) {
			return;
		}
		// Refresh the weak Activity reference the (once-registered) DisplayListener reads, so after an
		// Activity recreation the listener acts on the current Activity instead of a stale/destroyed one.
		currentActivityRef = new WeakReference<>(activity);
		try {
			ensureNativeLibrariesLoaded();
			nativeRegisterActivity(activity);
			registerDisplayListener(activity);
			Display currentDisplay = activity.getWindowManager().getDefaultDisplay();
			Log.i(TAG, BRIDGE_VERSION + ": Activity registered with the godot-xreal GDExtension on display "
					+ (currentDisplay == null ? -1 : currentDisplay.getDisplayId()));
		} catch (Throwable t) {
			Log.e(TAG, "nativeRegisterActivity failed", t);
		}
	}

	private static synchronized void registerDisplayListener(Activity activity) {
		if (displayListenerRegistered) {
			return;
		}
		// Resolve the DisplayManager from the application context, not the Activity: this listener is
		// registered once for the whole process and never unregistered, so capturing an Activity-bound
		// service would pin that Activity forever. The application context is a process-global singleton,
		// and DisplayManager's registration is process-wide regardless of which context wraps it.
		DisplayManager displayManager =
				(DisplayManager) activity.getApplicationContext().getSystemService(Context.DISPLAY_SERVICE);
		if (displayManager == null) {
			return;
		}
		displayListenerRegistered = true;
		displayManager.registerDisplayListener(new DisplayManager.DisplayListener() {
			@Override
			public void onDisplayAdded(int displayId) {
				Display display = displayManager.getDisplay(displayId);
				Log.i(TAG, BRIDGE_VERSION + ": display added "
						+ (display == null ? displayId : describeDisplay(display)));
				if (isXrealDisplay(display)) {
					xrealDisplayId = displayId;
					notifyGlassesConnected(displayId);
					// Read the current Activity from the static (not the captured param) so we never
					// touch an Activity destroyed since this listener was registered.
					Activity current = currentActivityRef.get();
					if (current != null) {
						current.runOnUiThread(() -> startCompanionOnXrealDisplayIfNeeded(current));
					}
				}
			}

			@Override
			public void onDisplayRemoved(int displayId) {
				Log.i(TAG, BRIDGE_VERSION + ": display removed " + displayId);
				if (displayId == xrealDisplayId) {
					xrealDisplayId = -1;
					notifyGlassesDisconnected(displayId);
				}
				companionLaunchRequested = false;
			}

			@Override
			public void onDisplayChanged(int displayId) {
				Display display = displayManager.getDisplay(displayId);
				if (isXrealDisplay(display)) {
					Log.i(TAG, BRIDGE_VERSION + ": XREAL display changed " + describeDisplay(display));
				}
			}
		}, new Handler(Looper.getMainLooper()));

		// onDisplayAdded is not fired for displays already present when the listener is
		// registered. Check now so NRFakeActivity launches even when the glasses were
		// connected before the app started.
		Display existing = findXrealDisplay(activity);
		if (existing != null) {
			Log.i(TAG, BRIDGE_VERSION + ": XREAL display already present at registration: "
					+ describeDisplay(existing));
			xrealDisplayId = existing.getDisplayId();
			notifyGlassesConnected(xrealDisplayId);
			activity.runOnUiThread(() -> startCompanionOnXrealDisplayIfNeeded(activity));
		}
	}

	/**
	 * Start a small companion Activity on the glasses display. This mirrors only the
	 * display-selection part of Unity's NRFakeActivity path without depending on UnityPlayer.
	 */
	public static synchronized void startCompanionOnXrealDisplayIfNeeded(Activity activity) {
		if (activity == null || companionLaunchRequested) {
			return;
		}
		Display currentDisplay = activity.getWindowManager().getDefaultDisplay();
		if (isXrealDisplay(currentDisplay)) {
			Log.i(TAG, BRIDGE_VERSION + ": already running on XREAL display "
					+ describeDisplay(currentDisplay));
			return;
		}
		Display xrealDisplay = findXrealDisplay(activity);
		if (xrealDisplay == null) {
			Log.i(TAG, BRIDGE_VERSION + ": no XREAL display available for companion Activity");
			return;
		}

		Intent intent = new Intent();
		intent.setComponent(new ComponentName(activity.getPackageName(),
				"ai.nreal.activitylife.NRFakeActivity"));
		intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_MULTIPLE_TASK);

		ActivityManager activityManager =
				(ActivityManager) activity.getSystemService(Context.ACTIVITY_SERVICE);
		if (activityManager != null
				&& !activityManager.isActivityStartAllowedOnDisplay(
						activity, xrealDisplay.getDisplayId(), intent)) {
			Log.w(TAG, BRIDGE_VERSION + ": Android refused companion Activity on display "
					+ describeDisplay(xrealDisplay));
			return;
		}

		ActivityOptions options = ActivityOptions.makeBasic();
		options.setLaunchDisplayId(xrealDisplay.getDisplayId());
		companionLaunchRequested = true;
		Log.i(TAG, BRIDGE_VERSION + ": starting companion Activity on display "
				+ describeDisplay(xrealDisplay));
		activity.startActivity(intent, options.toBundle());
	}

	/**
	 * Multi-resume: enable auto-enter Picture-in-Picture on the phone-side (display 0) Activity so that
	 * backgrounding the app enters PiP (a small tile on the phone) instead of stopping it. In PiP the
	 * Activity is paused-but-visible, so Godot's GL thread and its Surface stay alive and the XREAL
	 * glasses keep rendering instead of freezing (see docs/plans/background-render-plan.md). This calls
	 * the Android API directly (independent of Godot's own PiP gate); it needs
	 * android:supportsPictureInPicture on the launcher Activity (already in the manifest). No-op below
	 * API 31 (setAutoEnterEnabled) or when called for a non-display-0 Activity (the glasses companion).
	 * Driven from GDScript (demo/main.gd) alongside {@link #register}.
	 */
	public static void enableAutoEnterPiP(Activity activity) {
		if (activity == null || Build.VERSION.SDK_INT < Build.VERSION_CODES.S) {
			return;
		}
		try {
			Display display = activity.getWindowManager().getDefaultDisplay();
			if (display == null || display.getDisplayId() != Display.DEFAULT_DISPLAY) {
				return; // only the phone-side main Activity, not the glasses companion
			}
			PictureInPictureParams params = new PictureInPictureParams.Builder()
					.setAspectRatio(new Rational(16, 9))
					.setAutoEnterEnabled(true)
					.build();
			activity.setPictureInPictureParams(params);
			Log.i(TAG, BRIDGE_VERSION + ": auto-enter PiP enabled (display 0)");
		} catch (Throwable t) {
			Log.w(TAG, "enableAutoEnterPiP failed", t);
		}
	}

	private static native void nativeRegisterActivity(Activity activity);

	private static native void nativeOnGlassesConnected(int displayId);

	private static native void nativeOnGlassesDisconnected(int displayId);

	/** Forward a glasses connect event to native, tolerating a missing symbol (template drift). */
	private static void notifyGlassesConnected(int displayId) {
		try {
			nativeOnGlassesConnected(displayId);
		} catch (Throwable t) {
			Log.w(TAG, "nativeOnGlassesConnected unavailable", t);
		}
	}

	/** Forward a glasses disconnect event to native, tolerating a missing symbol. */
	private static void notifyGlassesDisconnected(int displayId) {
		try {
			nativeOnGlassesDisconnected(displayId);
		} catch (Throwable t) {
			Log.w(TAG, "nativeOnGlassesDisconnected unavailable", t);
		}
	}
}
