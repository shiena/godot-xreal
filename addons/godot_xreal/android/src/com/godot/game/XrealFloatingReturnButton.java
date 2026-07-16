package com.godot.game;

import android.app.Activity;
import android.app.ActivityOptions;
import android.app.Application;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.PixelFormat;
import android.os.Build;
import android.os.Bundle;
import android.provider.Settings;
import android.util.DisplayMetrics;
import android.util.Log;
import android.view.Display;
import android.view.Gravity;
import android.view.MotionEvent;
import android.view.View;
import android.view.ViewConfiguration;
import android.view.WindowManager;
import android.widget.ImageView;

/**
 * A floating on-phone "return" button for multi-resume: while the glasses app keeps running in the
 * background, a small tappable icon sits on the phone's screen; tapping it brings our app back to the
 * foreground.
 *
 * This is a Unity-free reimplementation of the reference NRSDK
 * {@code ai.nreal.activitylife.NRDefaultFloatingViewProxy} (see
 * {@code docs/archive/codex-floatingmanager-analysis.md}). The button is a **separate
 * {@link WindowManager.LayoutParams#TYPE_APPLICATION_OVERLAY} window** (its own Surface, on the phone's
 * default display) — it never touches the app's GL SurfaceView, so it cannot corrupt the Godot GL
 * surface the way an in-content-view overlay would. Window params are copied from the reference smali
 * ({@code NRDefaultFloatingViewProxy.smali:371-415}): 200x200 px, non-focusable, translucent, top-right.
 *
 * Wiring: {@link #init(Activity)} (called from {@link XrealBridge#register}) registers Activity
 * lifecycle callbacks so the button auto-shows when our main (display-0) Activity is stopped and
 * auto-hides when it starts. Tapping refocuses via a plain {@code startActivity} to our launcher on
 * display 0 — the exact non-Unity equivalent of {@code NRXRApp.startUnityActivityInternal}.
 *
 * Requires the {@code SYSTEM_ALERT_WINDOW} permission (user-granted at runtime; injected by
 * {@code export_plugin.gd}). If it is not granted, {@link #show()} is a logged no-op.
 */
public final class XrealFloatingReturnButton {
	private static final String TAG = "xreal";
	private static final int SIZE_PX = 200;

	private static Activity activity;
	private static View view;
	private static WindowManager.LayoutParams params;
	private static boolean added = false;
	private static boolean lifecycleRegistered = false;

	private XrealFloatingReturnButton() {}

	/** True when this app's manifest declares `nr_features=multiResume` (how the project enables it). */
	private static boolean isMultiResumeEnabled(Context ctx) {
		try {
			android.content.pm.ApplicationInfo ai = ctx.getPackageManager()
					.getApplicationInfo(ctx.getPackageName(), PackageManager.GET_META_DATA);
			if (ai.metaData == null) {
				return false;
			}
			Object v = ai.metaData.get("nr_features");
			return v != null && v.toString().toLowerCase().contains("multiresume");
		} catch (Throwable t) {
			return false;
		}
	}

	/** Store the phone-side Activity and register auto show/hide on its stop/start. Idempotent. */
	public static synchronized void init(Activity a) {
		if (a == null) {
			return;
		}
		// register() is called for BOTH the phone-side main Activity (display 0) and the glasses
		// companion (display 24). Only the display-0 Activity is the one whose backgrounding should
		// raise the return button, so ignore any other display's Activity here.
		int displayId;
		try {
			displayId = a.getWindowManager().getDefaultDisplay().getDisplayId();
		} catch (Throwable t) {
			displayId = -1;
		}
		if (displayId != Display.DEFAULT_DISPLAY) {
			return;
		}
		// Only meaningful when multi-resume is on (the glasses app keeps running in the background).
		// Gate on the manifest's `nr_features=multiResume` marker, which is how this project enables it.
		if (!isMultiResumeEnabled(a)) {
			Log.i(TAG, "floating: multiResume not enabled -> return button disabled");
			return;
		}
		activity = a;
		if (lifecycleRegistered) {
			return;
		}
		Application app = a.getApplication();
		if (app == null) {
			return;
		}
		lifecycleRegistered = true;
		app.registerActivityLifecycleCallbacks(new SimpleLifecycle() {
			@Override
			public void onActivityStopped(Activity act) {
				// Our main (display-0) Activity went to background while multi-resume keeps the
				// glasses app alive → show the return button on the phone.
				if (act == activity) {
					Log.i(TAG, "floating: main Activity stopped -> show()");
					show();
				}
			}

			@Override
			public void onActivityStarted(Activity act) {
				if (act == activity) {
					Log.i(TAG, "floating: main Activity started -> hide()");
					hide();
				}
			}
		});
		Log.i(TAG, "floating: init (lifecycle auto show/hide registered)");
	}

	/** Show the floating button (creating its overlay window on first use). Safe from any thread. */
	public static void show() {
		final Activity a = activity;
		if (a == null) {
			return;
		}
		a.runOnUiThread(XrealFloatingReturnButton::showOnUi);
	}

	/** Hide the floating button. Safe from any thread. */
	public static void hide() {
		final Activity a = activity;
		if (a == null) {
			return;
		}
		a.runOnUiThread(XrealFloatingReturnButton::hideOnUi);
	}

	private static void showOnUi() {
		Activity a = activity;
		if (a == null) {
			return;
		}
		if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M && !Settings.canDrawOverlays(a)) {
			Log.w(TAG, "floating: SYSTEM_ALERT_WINDOW not granted (canDrawOverlays=false); skipping");
			return;
		}
		WindowManager wm = a.getWindowManager();
		if (wm == null) {
			return;
		}
		try {
			if (view == null) {
				view = buildView(a);
			}
			if (!added) {
				params = buildParams(a);
				wm.addView(view, params);
				added = true;
				Log.i(TAG, "floating: overlay window added (TYPE_APPLICATION_OVERLAY, display "
						+ a.getWindowManager().getDefaultDisplay().getDisplayId() + ")");
			}
			view.setVisibility(View.VISIBLE);
		} catch (Throwable t) {
			Log.e(TAG, "floating: addView failed", t);
		}
	}

	private static void hideOnUi() {
		if (view != null && added) {
			view.setVisibility(View.GONE);
			Log.i(TAG, "floating: hidden");
		}
	}

	private static View buildView(Activity a) {
		ImageView iv = new ImageView(a);
		try {
			iv.setImageDrawable(a.getPackageManager().getApplicationIcon(a.getPackageName()));
		} catch (Throwable ignored) {
			// no icon available; the view stays transparent (the full window is still tappable).
		}
		// No background: the overlay window is TRANSLUCENT, so only the (circular) app icon shows;
		// the transparent corners of the 200x200 window remain part of the tap target.
		iv.setOnTouchListener(new DragTapListener(a));
		return iv;
	}

	/**
	 * Drag-to-move + tap-to-refocus, ported from the reference
	 * {@code NRDefaultFloatingViewProxy$CustomTouchHandler}: a move past {@code touchSlop} drags the
	 * overlay window (which follows the finger via {@code updateViewLayout}, so touch stays captured),
	 * releasing snaps to the nearest side edge; a release that never moved and lasted under 300 ms is a
	 * tap → {@link #onTap()}.
	 */
	private static final class DragTapListener implements View.OnTouchListener {
		private final WindowManager wm;
		private final int touchSlop;
		private final int screenW;
		private int startX, startY;
		private float downRawX, downRawY;
		private long downTime;
		private boolean moving;

		DragTapListener(Activity a) {
			wm = a.getWindowManager();
			touchSlop = ViewConfiguration.get(a).getScaledTouchSlop();
			DisplayMetrics dm = new DisplayMetrics();
			a.getWindowManager().getDefaultDisplay().getRealMetrics(dm);
			screenW = dm.widthPixels;
		}

		@Override
		public boolean onTouch(View v, MotionEvent e) {
			if (params == null) {
				return false;
			}
			switch (e.getActionMasked()) {
				case MotionEvent.ACTION_DOWN:
					startX = params.x;
					startY = params.y;
					downRawX = e.getRawX();
					downRawY = e.getRawY();
					downTime = e.getEventTime();
					moving = false;
					return true;
				case MotionEvent.ACTION_MOVE:
					int dx = (int) (e.getRawX() - downRawX);
					int dy = (int) (e.getRawY() - downRawY);
					if (!moving && Math.hypot(dx, dy) > touchSlop) {
						moving = true;
					}
					if (moving) {
						params.x = Math.max(0, Math.min(startX + dx, screenW - SIZE_PX));
						params.y = Math.max(0, startY + dy);
						updateLayout();
					}
					return true;
				case MotionEvent.ACTION_UP:
					if (!moving && (e.getEventTime() - downTime) < 300L) {
						onTap();
					} else if (moving) {
						// snap to the nearer side edge
						params.x = (params.x + SIZE_PX / 2 < screenW / 2) ? 0 : screenW - SIZE_PX;
						updateLayout();
					}
					return true;
				default:
					return false;
			}
		}

		private void updateLayout() {
			try {
				if (view != null && added) {
					wm.updateViewLayout(view, params);
				}
			} catch (Throwable t) {
				Log.w(TAG, "floating: updateViewLayout failed", t);
			}
		}
	}

	/**
	 * WindowManager params copied verbatim from the reference
	 * {@code NRDefaultFloatingViewProxy.smali:371-415}: a 200x200 TYPE_APPLICATION_OVERLAY window,
	 * FLAG_NOT_FOCUSABLE|FLAG_ALT_FOCUSABLE_IM (never steals focus, still receives taps), TRANSLUCENT,
	 * pinned to the top-right corner of the phone's default display.
	 */
	private static WindowManager.LayoutParams buildParams(Activity a) {
		int type = Build.VERSION.SDK_INT >= Build.VERSION_CODES.O
				? WindowManager.LayoutParams.TYPE_APPLICATION_OVERLAY
				: WindowManager.LayoutParams.TYPE_PHONE;
		WindowManager.LayoutParams params = new WindowManager.LayoutParams(
				SIZE_PX,
				SIZE_PX,
				type,
				WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE
						| WindowManager.LayoutParams.FLAG_ALT_FOCUSABLE_IM,
				PixelFormat.TRANSLUCENT);
		params.gravity = Gravity.TOP | Gravity.START;
		DisplayMetrics dm = new DisplayMetrics();
		a.getWindowManager().getDefaultDisplay().getRealMetrics(dm);
		params.x = Math.max(0, dm.widthPixels - SIZE_PX);
		params.y = 0;
		return params;
	}

	/** Tap → bring our app back to the foreground on the phone (display 0). */
	private static void onTap() {
		Activity a = activity;
		if (a == null) {
			return;
		}
		Log.i(TAG, "floating: tapped -> refocus app on display 0");
		try {
			Intent intent = a.getPackageManager().getLaunchIntentForPackage(a.getPackageName());
			if (intent == null) {
				Log.w(TAG, "floating: no launch intent for " + a.getPackageName());
				return;
			}
			intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
			Bundle opts = null;
			if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
				ActivityOptions o = ActivityOptions.makeBasic();
				o.setLaunchDisplayId(Display.DEFAULT_DISPLAY); // 0 = phone
				opts = o.toBundle();
			}
			a.startActivity(intent, opts);
		} catch (Throwable t) {
			Log.e(TAG, "floating: startActivity (refocus) failed", t);
		}
	}

	/** Empty {@link Application.ActivityLifecycleCallbacks} base so we override only what we need. */
	private abstract static class SimpleLifecycle implements Application.ActivityLifecycleCallbacks {
		@Override public void onActivityCreated(Activity a, Bundle b) {}
		@Override public void onActivityStarted(Activity a) {}
		@Override public void onActivityResumed(Activity a) {}
		@Override public void onActivityPaused(Activity a) {}
		@Override public void onActivityStopped(Activity a) {}
		@Override public void onActivitySaveInstanceState(Activity a, Bundle b) {}
		@Override public void onActivityDestroyed(Activity a) {}
	}
}
