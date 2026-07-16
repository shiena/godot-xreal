package com.godot.game;

import android.app.Activity;
import android.graphics.Color;
import android.os.Bundle;
import android.util.Log;
import android.view.Gravity;
import android.view.WindowManager;
import android.widget.TextView;

/**
 * Minimal non-Unity companion Activity launched on the XREAL secondary display.
 *
 * Unity's nractivitylife package starts NRFakeActivity on the glasses display and
 * resumes Unity from there. We cannot reuse it because it directly references
 * UnityPlayer, so this Activity only supplies the Android display/lifecycle shape.
 */
public class XrealCompanionActivity extends Activity {
	private static final String TAG = "xreal";

	@Override
	protected void onCreate(Bundle savedInstanceState) {
		super.onCreate(savedInstanceState);
		getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON
				| WindowManager.LayoutParams.FLAG_FULLSCREEN);
		TextView view = new TextView(this);
		view.setBackgroundColor(Color.MAGENTA);
		view.setTextColor(Color.WHITE);
		view.setTextSize(96.0f);
		view.setGravity(Gravity.CENTER);
		view.setText("XREAL\nCOMPANION\nDISPLAY "
				+ getWindowManager().getDefaultDisplay().getDisplayId());
		setContentView(view);
		Log.i(TAG, "companion Activity created on display "
				+ getWindowManager().getDefaultDisplay().getDisplayId());
		XrealBridge.register(this);
	}

	@Override
	protected void onResume() {
		super.onResume();
		Log.i(TAG, "companion Activity resumed on display "
				+ getWindowManager().getDefaultDisplay().getDisplayId());
		XrealBridge.register(this);
	}
}
