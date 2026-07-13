extends Node3D
## Phone-as-3D-pointer for XREAL (Phase C, path B). The NRController fused pose isn't available on
## this host, but its raw IMU is live, so we fuse orientation here from accelerometer (gravity →
## pitch/roll, drift-free) + gyroscope (yaw/short-term, integrated). A complementary filter keeps
## pitch/roll locked to gravity while the gyro carries yaw (recenter to re-align "forward").
##
## Feed it each frame via `update_imu(accel, gyro, dt, head_transform)` with the NRController sensors.
## Phone IMU frame: X=right, Y=top, Z=out of screen (verified on device). It raycasts along the beam,
## highlights whatever it hits, and `select()` (from a trigger) clicks it. `recenter()` makes the
## current aim forward. Gyro drift is suppressed by learning the resting bias + a small deadzone.

## Complementary-filter gain: how strongly gravity corrects pitch/roll each frame (0 = gyro only).
@export var gravity_gain := 0.06
## Ray length (m).
@export var ray_length := 6.0
## Where the beam originates relative to the head. On the glasses buffer +Y reads as down, so a
## positive Y puts the origin at bottom-right (right hand); flipped by `set_hand`.
@export var hand_offset := Vector3(0.28, 0.32, -0.3)
## Gyro drift suppression: rate (rad/s, after bias) below this counts as noise; bias learn rate.
@export var gyro_deadzone := 0.012
@export var bias_learn := 0.02

signal aim_changed(origin: Vector3, direction: Vector3)

var _q := Quaternion.IDENTITY        # fused phone orientation (phone frame -> filter frame)
var _ref := Quaternion.IDENTITY      # recenter offset
var _have_ref := false
var _gyro_bias := Vector3.ZERO       # learned resting gyro bias (drift suppression)
var _ray: MeshInstance3D
var _tip: MeshInstance3D
var _raycast: RayCast3D
var _hover: MeshInstance3D           # box currently under the ray
var _hover_emission := Color.BLACK   # its emission before we highlighted it

func _ready() -> void:
	var beam := BoxMesh.new()
	beam.size = Vector3(0.012, 0.012, ray_length)
	_ray = MeshInstance3D.new()
	_ray.name = "PointerBeam"
	_ray.mesh = beam
	_ray.position = Vector3(0, 0, -ray_length * 0.5)
	var mat := StandardMaterial3D.new()
	mat.albedo_color = Color(0.2, 1.0, 0.45, 0.85)
	mat.emission_enabled = true
	mat.emission = Color(0.2, 1.0, 0.45)
	mat.transparency = BaseMaterial3D.TRANSPARENCY_ALPHA
	_ray.material_override = mat
	add_child(_ray)

	var tip := SphereMesh.new()
	tip.radius = 0.07
	tip.height = 0.14
	_tip = MeshInstance3D.new()
	_tip.name = "PointerTip"
	_tip.mesh = tip
	_tip.position = Vector3(0, 0, -ray_length)
	_tip.material_override = mat
	add_child(_tip)

	_raycast = RayCast3D.new()
	_raycast.target_position = Vector3(0, 0, -ray_length)
	_raycast.enabled = true
	add_child(_raycast)

## Fuse one IMU sample, place the beam at the hand offset, aim it, and raycast.
func update_imu(accel: Vector3, gyro: Vector3, dt: float, head_transform: Transform3D) -> void:
	# Gyro drift suppression: learn the resting bias while nearly still, then subtract + deadzone.
	if gyro.length() < 0.1:
		_gyro_bias = _gyro_bias.lerp(gyro, bias_learn)
	var g := gyro - _gyro_bias
	if g.length() < gyro_deadzone:
		g = Vector3.ZERO

	# 1) integrate the (bias-corrected) gyro
	var wl := g.length()
	if wl > 0.000001 and dt > 0.0:
		_q = (_q * Quaternion(g / wl, wl * dt)).normalized()
	# 2) correct pitch/roll toward gravity: nudge predicted world-up onto measured up (phone frame)
	if accel.length() > 1.0:
		var up_meas := accel.normalized()
		var up_pred := (_q.inverse() * Vector3.UP).normalized()
		var axis := up_pred.cross(up_meas)
		var al := axis.length()
		if al > 0.000001:
			_q = (_q * Quaternion(axis / al, asin(clampf(al, -1.0, 1.0)) * gravity_gain)).normalized()

	if not _have_ref:
		return
	# Relative rotation from the recenter pose, re-expressed in Godot's frame; yaw was correct but
	# pitch inverted, so flip the pitch (X euler) while keeping yaw/roll.
	var rel := _ref * _q
	var remap := Basis(Vector3(1, 0, 0), Vector3(0, 0, -1), Vector3(0, 1, 0))
	var e := (remap * Basis(rel) * remap.transposed()).get_euler()
	var aim_basis := Basis.from_euler(Vector3(-e.x, e.y, e.z))
	# Origin at a "hand" offset from the head (not the eye/camera).
	var origin := head_transform.origin + head_transform.basis * hand_offset
	global_transform = Transform3D(aim_basis, origin)

	_raycast.force_raycast_update()
	_update_hover()
	aim_changed.emit(origin, -aim_basis.z)

func _update_hover() -> void:
	var hit: MeshInstance3D = null
	if _raycast.is_colliding():
		var collider := _raycast.get_collider()
		if collider:
			hit = collider.get_parent() as MeshInstance3D  # StaticBody3D -> box
		_tip.global_position = _raycast.get_collision_point()
	else:
		_tip.position = Vector3(0, 0, -ray_length)
	if hit != _hover:
		_restore_hover()
		_hover = hit
		if _hover:
			var m := _hover.material_override as StandardMaterial3D
			if m:
				_hover_emission = m.emission
				m.emission_enabled = true
				m.emission = m.albedo_color * 0.8

func _restore_hover() -> void:
	if _hover:
		var m := _hover.material_override as StandardMaterial3D
		if m:
			m.emission = _hover_emission
			m.emission_enabled = _hover_emission.r + _hover_emission.g + _hover_emission.b > 0.001
	_hover = null

## Click the hovered object (wire to a trigger). Recolors it as visible feedback.
func select() -> void:
	if _hover:
		var m := _hover.material_override as StandardMaterial3D
		if m:
			m.albedo_color = Color.from_hsv(randf(), 0.75, 0.95)
			_hover_emission = Color.BLACK  # refresh so hover highlight recomputes

## Switch the beam origin between the right hand (default) and the left hand.
func set_hand(is_right: bool) -> void:
	hand_offset.x = absf(hand_offset.x) * (1.0 if is_right else -1.0)

## Make the current phone orientation the forward direction.
func recenter() -> void:
	_ref = _q.inverse()
	_have_ref = true
