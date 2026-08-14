extends Node3D
## Skinned hand models on the XREAL hand trackers (Air 2 Ultra), as a drop-in feature component.
## Give it a hand model per side and it builds Godot's standard hand rig for each: an `XRNode3D`
## bound to the tracker, the model under it, and an `XRHandModifier3D` on the model's `Skeleton3D`.
## The hands then sit on the real hands, articulate with them, and disappear when tracking is lost.
##
## The models are not shipped with the addon, because they are somebody else's art with its own
## licence. `docs/user/README.md` names a set that works and walks through wiring it up.
##
## Not world-locked, unlike most components here: add it under your `XROrigin3D`, and leave its own
## transform alone. `XRNode3D` reads each tracker's palm pose, which is relative to the tracking
## origin, so the parent chain has to carry the origin's transform and nothing else. (`xreal_hands`,
## the joint spheres, is the opposite case: those transforms are already world-space, so that
## component belongs under a world-fixed node.)
##
## On glasses without hand tracking, and while the hands are out of view, no pose arrives and the
## models stay hidden. Nothing has to switch this component off for a One or a One Pro.

## The left hand's model. A glTF or a scene whose skeleton uses Godot's `Left<bone>` bone names;
## see the doc link above for what that means and which models qualify.
@export var left_model: PackedScene

## The right hand's model, with `Right<bone>` bone names.
@export var right_model: PackedScene

## Applied to every mesh in both models. Leave null to keep each model's own materials.
##
## Unshaded is usually the right choice for AR: the glasses composite additively onto the real
## world, so a hand lit by the scene's lights reads as a dark smear whenever the scene is dark.
@export var material_override: Material

## What the modifier drives. `BONE_UPDATE_FULL` moves and rotates every bone, so the model takes on
## the wearer's finger lengths; `BONE_UPDATE_ROTATION_ONLY` keeps the model's own proportions.
@export_enum("Full", "Rotation only") var bone_update: int = 0


func _ready() -> void:
	if XrealShared.is_native_runtime():
		var src := int(XrealShared.read_setting("xreal/input_source", -1))
		# Same gate as xreal_hands: hand tracking needs the SDK's Hands bit, which is off by default
		# because asking for it costs ~878 ms of cold start, and dropping this scene in cannot turn
		# it on for you. Parenthesise the mask; in GDScript `==` binds tighter than `&`.
		if src < 0 or (src & 2) == 0:
			var shown := "SDK Default (controller only)" if src < 0 else str(src)
			push_warning(("[xreal-hand-models] hand tracking is off: set Project Settings > " +
				"xreal/input_source to \"Controller And Hands\" (currently %s). It defaults to " +
				"controller-only because the Hands bit costs ~878 ms of startup.") % shown)
		XrealShared.get_hand_tracker(get_tree())  # the trackers register with XRServer in its ready
	if not (get_parent() is XROrigin3D):
		push_warning("[xreal-hand-models] expected an XROrigin3D parent; the hands will be off by " +
			"whatever transform sits between this node and the tracking origin.")
	_build(left_model, &"/user/hand_tracker/left", "left_model")
	_build(right_model, &"/user/hand_tracker/right", "right_model")


## Build one hand. Safe to run before the trackers exist: `XRNode3D` and `XRHandModifier3D` both
## look their tracker up by name, and `XRNode3D` rebinds on `XRServer.tracker_added`, which is what
## the XREAL trackers do a frame or so later (XrealShared adds the node deferred).
func _build(scene: PackedScene, tracker: StringName, export_name: String) -> void:
	if scene == null:
		push_warning("[xreal-hand-models] %s is empty, so that hand is not built." % export_name)
		return

	var node := XRNode3D.new()
	node.name = "%sHand" % tracker.get_file().capitalize()
	node.tracker = tracker
	node.show_when_tracked = true
	# Start hidden rather than trusting show_when_tracked alone. It only takes effect once XRServer
	# has a primary interface, and on XREAL that arrives with the glasses, after this _ready. In
	# that window the node would draw its skeleton in the bind pose: a hand-shaped lump parked at
	# the origin, right in front of the eyes.
	node.visible = false
	add_child(node)

	var model := scene.instantiate() as Node3D
	if model == null:
		push_warning("[xreal-hand-models] %s does not instantiate to a Node3D." % export_name)
		return
	node.add_child(model)

	var skeleton := model.find_child("Skeleton3D", true, false) as Skeleton3D
	if skeleton == null:
		push_warning(("[xreal-hand-models] %s has no Skeleton3D, so nothing drives it. A hand " +
			"model has to be a skinned mesh.") % export_name)
		return
	var modifier := XRHandModifier3D.new()
	modifier.hand_tracker = tracker
	modifier.bone_update = bone_update
	skeleton.add_child(modifier)

	if material_override:
		_apply_material(model)


func _apply_material(node: Node) -> void:
	var mesh := node as MeshInstance3D
	if mesh:
		mesh.material_override = material_override
	for child: Node in node.get_children():
		_apply_material(child)
