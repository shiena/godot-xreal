package com.godot.game;

import android.app.Activity;
import android.content.Intent;
import android.media.projection.MediaProjectionManager;
import android.os.Bundle;
import android.util.Log;

/**
 * Collects screen-capture consent and hands the result to {@link XrealProjectionService}.
 *
 * A separate transparent Activity rather than a hook in the Godot Activity: the consent dialog is
 * driven by startActivityForResult, and the Godot Android template gives a GDExtension no result
 * callback of its own. This Activity exists only for the length of the dialog.
 *
 * It does NOT create the MediaProjection itself. On API 34+ that is only legal once a foreground
 * service of type mediaProjection is running, so the raw result is forwarded to the service, which
 * foregrounds itself first.
 */
public final class XrealProjectionActivity extends Activity {
	private static final int REQUEST_CODE = 0x58524d50; // 'XRMP'

	@Override
	protected void onCreate(Bundle savedInstanceState) {
		super.onCreate(savedInstanceState);
		MediaProjectionManager manager =
				(MediaProjectionManager) getSystemService(MEDIA_PROJECTION_SERVICE);
		if (manager == null) {
			Log.w(XrealProjection.TAG, "no MediaProjectionManager on this device");
			XrealProjection.onProjectionDenied();
			finish();
			return;
		}
		try {
			startActivityForResult(manager.createScreenCaptureIntent(), REQUEST_CODE);
		} catch (Throwable t) {
			Log.w(XrealProjection.TAG, "cannot show the screen-capture consent dialog", t);
			XrealProjection.onProjectionDenied();
			finish();
		}
	}

	@Override
	protected void onActivityResult(int requestCode, int resultCode, Intent data) {
		super.onActivityResult(requestCode, resultCode, data);
		if (requestCode != REQUEST_CODE) {
			return;
		}
		if (resultCode != RESULT_OK || data == null) {
			XrealProjection.onProjectionDenied();
			finish();
			return;
		}
		Intent service = new Intent(this, XrealProjectionService.class);
		service.putExtra(XrealProjectionService.EXTRA_RESULT_CODE, resultCode);
		service.putExtra(XrealProjectionService.EXTRA_RESULT_DATA, data);
		startForegroundService(service);
		finish();
	}
}
