package ai.nreal.activitylife;

import android.app.Activity;
import android.os.Bundle;
import android.util.Log;
import android.view.Window;

/**
 * Non-Unity stand-in for the NR runtime's lifecycle Activity.
 *
 * The NR loader (nr_features "multiResume") starts an Activity with exactly this
 * fully-qualified name; without a class behind it the app dies at startup with
 * ClassNotFoundException. Unity's own NRFakeActivity (nractivitylife*.aar) cannot be
 * shipped because it references UnityPlayer, so this stub supplies just the Android
 * lifecycle shape. onBackPressed deliberately does NOT call super: the back key on the
 * glasses display must not finish the Activity.
 *
 * (Reconstructed 2026-07-15 from the previously shipped xreal_bridge.jar bytecode —
 * this source was never committed with the original jar.)
 */
public class NRFakeActivity extends Activity {
	private static final String TAG = "NRFakeActivity";

	@Override
	protected void onCreate(Bundle savedInstanceState) {
		requestWindowFeature(Window.FEATURE_NO_TITLE);
		super.onCreate(savedInstanceState);
		getWindow().setBackgroundDrawable(null);
		Log.i(TAG, "onCreate on display "
				+ getWindowManager().getDefaultDisplay().getDisplayId());
	}

	@Override
	protected void onResume() {
		super.onResume();
		Log.i(TAG, "onResume");
	}

	@Override
	protected void onPause() {
		super.onPause();
		Log.i(TAG, "onPause");
	}

	@Override
	protected void onDestroy() {
		Log.i(TAG, "onDestroy");
		super.onDestroy();
	}

	@Override
	public void onBackPressed() {
		Log.i(TAG, "OnBackPressed");
	}
}
