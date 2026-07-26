class_name XrealAndroidBridge
extends Object
## Bootstrap for the XrealBridge Java helper (addons/godot_xreal/android/src/…/XrealBridge.java).
## It registers the bridge on the activity, moves the companion window onto the XREAL display when
## needed, and enables auto-enter Picture-in-Picture so the glasses keep rendering while the app
## is backgrounded on the phone (multi-resume). Call register() once at startup; see demo/main.gd.
## The Java methods are idempotent, and this is the Godot-side fallback for template drift.
##
## It is also the single place that reaches Godot's built-in AndroidRuntime plugin. Everything else
## in the addon, and in the app, should ask [method get_activity] rather than poke
## `Engine.get_singleton("AndroidRuntime")` itself, which handles the off-Android and
## missing-singleton cases once.

## Bridge methods that must still exist for [method register] to mean anything. They are checked
## because a JavaClass call to a method that is NOT there raises no error: it returns null, so the
## whole bootstrap would silently do nothing. That is exactly how a removed
## XrealBridge.saveToGallery kept "failing" quietly until the dead caller was found.
const _bridge_methods := ["register", "startCompanionOnXrealDisplayIfNeeded", "enableAutoEnterPiP"]

## Godot's built-in AndroidRuntime plugin, or null everywhere it does not exist.
static func _runtime() -> Object:
	if not OS.has_feature("android") or not Engine.has_singleton(&"AndroidRuntime"):
		return null
	return Engine.get_singleton(&"AndroidRuntime")

## The host Activity, or null off Android and while the Android runtime is unavailable. Nearly
## every Android API reached through JavaClassWrapper needs it as its Context.
static func get_activity() -> Object:
	var runtime := _runtime()
	return runtime.getActivity() if runtime != null else null

static func register() -> void:
	var runtime := _runtime()
	var activity := get_activity()
	if runtime == null or activity == null:
		return

	var bridge = JavaClassWrapper.wrap("com.godot.game.XrealBridge")
	if bridge == null:
		return
	for method in _bridge_methods:
		if not bridge.has_java_method(method):
			push_error(("[xreal] XrealBridge.%s is missing: the Java sources staged into the "
				+ "gradle build template are out of sync with this addon, so the glasses "
				+ "bootstrap did not run.") % method)
			return

	var register_bridge := func() -> void:
		bridge.register(activity)
		bridge.startCompanionOnXrealDisplayIfNeeded(activity)
		bridge.enableAutoEnterPiP(activity)

	activity.runOnUiThread(runtime.createRunnableFromGodotCallable(register_bridge))
