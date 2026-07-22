package com.godot.game;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.media.projection.MediaProjectionManager;
import android.os.Build;
import android.os.IBinder;
import android.util.Log;

/**
 * Foreground service that owns the media projection while app audio is being captured.
 *
 * Android 14 refuses {@code getMediaProjection()} unless a foreground service of type
 * {@code mediaProjection} is already running, so the projection is created here — after
 * startForeground — rather than in the Activity that collected consent. The service then does
 * nothing but stay alive: the capture itself happens inside libmedia_codec, which builds its own
 * AudioRecord from the projection.
 */
public final class XrealProjectionService extends Service {
	public static final String EXTRA_RESULT_CODE = "com.godot.game.xreal.RESULT_CODE";
	public static final String EXTRA_RESULT_DATA = "com.godot.game.xreal.RESULT_DATA";

	private static final String CHANNEL_ID = "xreal_capture";
	private static final int NOTIFICATION_ID = 0x5852;

	@Override
	public IBinder onBind(Intent intent) {
		return null;
	}

	@Override
	public int onStartCommand(Intent intent, int flags, int startId) {
		if (intent == null || !intent.hasExtra(EXTRA_RESULT_DATA)) {
			stopSelf();
			return START_NOT_STICKY;
		}
		try {
			startForeground(NOTIFICATION_ID, buildNotification(),
					ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION);
		} catch (Throwable t) {
			Log.e(XrealProjection.TAG, "cannot start the capture foreground service", t);
			XrealProjection.onProjectionDenied();
			stopSelf();
			return START_NOT_STICKY;
		}

		MediaProjectionManager manager =
				(MediaProjectionManager) getSystemService(MEDIA_PROJECTION_SERVICE);
		int resultCode = intent.getIntExtra(EXTRA_RESULT_CODE, RESULT_CANCELED_FALLBACK);
		Intent resultData = intent.getParcelableExtra(EXTRA_RESULT_DATA);
		try {
			// Only legal now that we are foregrounded, and only once per consent result.
			XrealProjection.onProjectionReady(manager.getMediaProjection(resultCode, resultData));
		} catch (Throwable t) {
			Log.e(XrealProjection.TAG, "getMediaProjection failed", t);
			XrealProjection.onProjectionDenied();
			stopSelf();
		}
		return START_NOT_STICKY;
	}

	/** Activity.RESULT_CANCELED, spelled out so this file needs no Activity import. */
	private static final int RESULT_CANCELED_FALLBACK = 0;

	private Notification buildNotification() {
		NotificationManager notifications = getSystemService(NotificationManager.class);
		if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && notifications != null) {
			NotificationChannel channel = new NotificationChannel(
					CHANNEL_ID, "XREAL capture", NotificationManager.IMPORTANCE_LOW);
			channel.setShowBadge(false);
			notifications.createNotificationChannel(channel);
		}
		return new Notification.Builder(this, CHANNEL_ID)
				.setContentTitle("XREAL capture")
				.setContentText("Recording app audio")
				.setSmallIcon(android.R.drawable.presence_video_online)
				.setOngoing(true)
				.build();
	}
}
