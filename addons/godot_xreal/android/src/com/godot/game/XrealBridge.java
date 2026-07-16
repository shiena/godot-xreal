package com.godot.game;

import android.app.Activity;
import android.app.ActivityManager;
import android.app.ActivityOptions;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.hardware.display.DisplayManager;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;
import android.view.Display;
import android.util.DisplayMetrics;

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
	/// Display id of the XREAL glasses while connected (-1 = none); used to recognise its removal,
	/// since onDisplayRemoved cannot query the (already gone) Display.
	private static int xrealDisplayId = -1;

	private XrealBridge() {}

	static Display findXrealDisplay(Context context) {
		DisplayManager displayManager = (DisplayManager) context.getSystemService(Context.DISPLAY_SERVICE);
		if (displayManager == null) {
			return null;
		}

		Display[] displays = displayManager.getDisplays();
		Display fallback = null;
		for (Display display : displays) {
			if (display.getDisplayId() == Display.DEFAULT_DISPLAY) {
				continue;
			}
			if (isXrealDisplay(display)) {
				return display;
			}
			if (fallback == null) {
				fallback = display;
			}
		}
		return fallback;
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
		loadNative("nr_loader");
		loadNative("nr_api");
		loadNative("XREALNativeSessionManager");
		loadNative("XREALXRPlugin");
		loadNative("godot_xreal");
		nativeLibrariesLoaded = true;
	}

	private static void loadNative(String name) {
		try {
			System.loadLibrary(name);
			Log.i(TAG, "loaded lib" + name + ".so");
		} catch (Throwable t) {
			Log.e(TAG, "Unable to load lib" + name + ".so", t);
		}
	}

	/** Publish the Activity to native code (safe to call more than once). */
	public static void register(Activity activity) {
		if (activity == null) {
			return;
		}
		try {
			ensureNativeLibrariesLoaded();
			nativeRegisterActivity(activity);
			registerDisplayListener(activity);
			// Multi-resume: auto show/hide a floating "return" button on the phone while the
			// glasses app runs in the background (see XrealFloatingReturnButton).
			XrealFloatingReturnButton.init(activity);
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
		DisplayManager displayManager = (DisplayManager)activity.getSystemService(Context.DISPLAY_SERVICE);
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
					activity.runOnUiThread(() -> startCompanionOnXrealDisplayIfNeeded(activity));
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
