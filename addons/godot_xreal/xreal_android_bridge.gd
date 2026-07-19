class_name XrealAndroidBridge
extends Object
## Bootstrap for the XrealBridge Java helper (addons/godot_xreal/android/src/…/XrealBridge.java):
## registers the bridge on the activity, moves the companion window onto the XREAL display when
## needed, and enables auto-enter Picture-in-Picture so the glasses keep rendering while the app
## is backgrounded on the phone (multi-resume). Call register() once at startup (see demo/main.gd).
## The Java methods are idempotent; this is the Godot-side fallback for template drift.

static func register() -> void:
	if not OS.has_feature("android"):
		return
	if not Engine.has_singleton(&"AndroidRuntime"):
		return

	var runtime := Engine.get_singleton(&"AndroidRuntime")
	if runtime == null:
		return
	var activity = runtime.getActivity()
	if activity == null:
		return

	var bridge = JavaClassWrapper.wrap("com.godot.game.XrealBridge")
	if bridge == null:
		return

	var register_bridge := func() -> void:
		bridge.register(activity)
		bridge.startCompanionOnXrealDisplayIfNeeded(activity)
		bridge.enableAutoEnterPiP(activity)

	activity.runOnUiThread(runtime.createRunnableFromGodotCallable(register_bridge))
