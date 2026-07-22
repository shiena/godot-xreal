package com.godot.game;

import android.app.Activity;
import android.content.Intent;
import android.media.projection.MediaProjection;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;

/**
 * Holds the Android {@link MediaProjection} the XREAL encoder needs to capture app ("internal")
 * audio, and hands it to native code.
 *
 * Why this exists: {@code addInternalAudio:true} in the encoder config does not mean "expect pushed
 * PCM" — reverse engineering libmedia_codec.so showed it builds an
 * {@code AudioPlaybackCaptureConfiguration} from a MediaProjection and opens its own AudioRecord
 * (see docs/archive/codex-audio-mix-analysis.md). Without a projection that capture cannot start, so
 * the encoder's own mixer has nothing to add to the microphone blocks.
 *
 * Consent is a two-step Android dance, and the order matters on API 34+: the projection may only be
 * created once a foreground service of type {@code mediaProjection} is already running. So
 * {@link XrealProjectionActivity} collects consent and {@link XrealProjectionService} starts itself
 * in the foreground before creating the projection.
 *
 * Driven from GDScript via JavaClassWrapper; nothing here is on a hot path.
 */
public final class XrealProjection {
	static final String TAG = "xreal";

	private static volatile MediaProjection projection;
	private static volatile boolean requestInFlight = false;

	private XrealProjection() {}

	/**
	 * Ask the user for screen-capture consent, unless a projection is already held or a request is
	 * already on screen. Returns immediately — poll {@link #isReady()}.
	 */
	public static void request(Activity activity) {
		if (activity == null || projection != null || requestInFlight) {
			return;
		}
		requestInFlight = true;
		Intent intent = new Intent(activity, XrealProjectionActivity.class);
		intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
		activity.startActivity(intent);
	}

	/** Whether app audio can be captured, i.e. whether consent has been granted and not revoked. */
	public static boolean isReady() {
		return projection != null;
	}

	/** Give up the projection and stop the foreground service. Safe when never granted. */
	public static void release(Activity activity) {
		MediaProjection held = projection;
		projection = null;
		nativeClearMediaProjection();
		if (held != null) {
			try {
				held.stop();
			} catch (Throwable t) {
				Log.w(TAG, "MediaProjection.stop failed", t);
			}
		}
		if (activity != null) {
			activity.stopService(new Intent(activity, XrealProjectionService.class));
		}
	}

	/** Called by {@link XrealProjectionService} once the projection exists. */
	static void onProjectionReady(MediaProjection mediaProjection) {
		requestInFlight = false;
		if (mediaProjection == null) {
			Log.w(TAG, "media projection request produced nothing");
			return;
		}
		// API 34 wants a callback registered before the projection is used, and we want to know when
		// the user revokes it from the status bar - a stale projection would silently capture nothing.
		try {
			mediaProjection.registerCallback(new MediaProjection.Callback() {
				@Override
				public void onStop() {
					Log.i(TAG, "media projection stopped by the system or the user");
					projection = null;
					nativeClearMediaProjection();
				}
			}, new Handler(Looper.getMainLooper()));
		} catch (Throwable t) {
			Log.w(TAG, "registerCallback failed", t);
		}
		projection = mediaProjection;
		try {
			nativeSetMediaProjection(mediaProjection);
			Log.i(TAG, "media projection ready; app audio capture is available");
		} catch (Throwable t) {
			Log.e(TAG, "nativeSetMediaProjection failed", t);
		}
	}

	/** Called when consent was declined or the flow failed, so a later attempt can retry. */
	static void onProjectionDenied() {
		requestInFlight = false;
		Log.i(TAG, "media projection denied; recordings will carry microphone audio only");
	}

	private static native void nativeSetMediaProjection(Object mediaProjection);

	private static void nativeClearMediaProjection() {
		try {
			nativeSetMediaProjection(null);
		} catch (Throwable t) {
			Log.w(TAG, "nativeSetMediaProjection(null) unavailable", t);
		}
	}
}
