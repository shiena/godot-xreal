extends Node3D
## Phone-as-3D-pointer for XREAL (Phase C, path B). The NRController fused pose isn't available on
## this host, but its raw IMU is live, so we fuse orientation here from accelerometer (gravity →
## pitch/roll, drift-free) + gyroscope (yaw/short-term, integrated). A complementary filter keeps
## pitch/roll locked to gravity while the gyro carries yaw (recenter to re-align "forward").
##
## Feed it each frame via `update_imu(accel, gyro, dt)` with the NRController sensors (Godot's own
## `Input.get_gyroscope()` etc. return zero on this host). Phone IMU frame: X=right, Y=top, Z=out of
## screen (verified on device). `recenter()` makes the current aim the forward direction.

## Complementary-filter gain: how strongly gravity corrects pitch/roll each frame (0 = gyro only).
@export var gravity_gain := 0.06
## Ray length (m).
@export var ray_length := 6.0
## Where the beam originates relative to the head. On the glasses buffer +Y reads as down, so a
## positive Y puts the origin at bottom-right (right hand).
@export var hand_offset := Vector3(0.28, 0.32, -0.3)

signal aim_changed(origin: Vector3, direction: Vector3)

var _q := Quaternion.IDENTITY        # fused phone orientation (phone frame -> filter frame)
var _ref := Quaternion.IDENTITY      # recenter offset
var _ray: MeshInstance3D
var _tip: MeshInstance3D
var _have_ref := false

func _ready() -> void:
	var beam := BoxMesh.new()
	beam.size = Vector3(0.012, 0.012, ray_length)
	_ray = MeshInstance3D.new()
	_ray.name = "PointerBeam"
	_ray.mesh = beam
	_ray.position = Vector3(0, 0, -ray_length * 0.5) # extend along -Z (Godot forward)
	var mat := StandardMaterial3D.new()
	mat.albedo_color = Color(0.2, 1.0, 0.45, 0.85)
	mat.emission_enabled = true
	mat.emission = Color(0.2, 1.0, 0.45)
	mat.transparency = BaseMaterial3D.TRANSPARENCY_ALPHA
	_ray.material_override = mat
	add_child(_ray)

	var tip := SphereMesh.new()
	tip.radius = 0.06
	tip.height = 0.12
	_tip = MeshInstance3D.new()
	_tip.name = "PointerTip"
	_tip.mesh = tip
	_tip.position = Vector3(0, 0, -ray_length)
	_tip.material_override = mat
	add_child(_tip)

## Fuse one IMU sample and re-aim the beam. `accel` = proper acceleration (m/s^2, gravity up),
## `gyro` = angular rate (rad/s) in the phone frame.
func update_imu(accel: Vector3, gyro: Vector3, dt: float) -> void:
	# 1) integrate gyro (phone-frame angular velocity)
	var wl := gyro.length()
	if wl > 0.000001 and dt > 0.0:
		_q = (_q * Quaternion(gyro / wl, wl * dt)).normalized()
	# 2) correct pitch/roll toward gravity: nudge predicted world-up onto measured up (in phone frame)
	if accel.length() > 1.0:
		var up_meas := accel.normalized()
		var up_pred := (_q.inverse() * Vector3.UP).normalized()
		var axis := up_pred.cross(up_meas)
		var al := axis.length()
		if al > 0.000001:
			var angle := asin(clampf(al, -1.0, 1.0)) * gravity_gain
			_q = (_q * Quaternion(axis / al, angle)).normalized()

	if not _have_ref:
		return
	# Relative rotation from the recenter pose (IMU frame), re-expressed in Godot's frame by
	# conjugation. Remap for a flat-screen-up neutral: IMU +X=right→+X, +Y(top edge)=forward→-Z,
	# +Z(screen normal)=up→+Y. This makes tilting the top edge up rotate the beam up (not down).
	var rel := _ref * _q
	var remap := Basis(Vector3(1, 0, 0), Vector3(0, 0, -1), Vector3(0, 1, 0))
	# Yaw was correct but pitch came out inverted, so flip the pitch (X euler) while keeping yaw/roll.
	var e := (remap * Basis(rel) * remap.transposed()).get_euler()
	global_transform.basis = Basis.from_euler(Vector3(-e.x, e.y, e.z))
	aim_changed.emit(global_position, -global_transform.basis.z)

## Make the current phone orientation the forward direction.
func recenter() -> void:
	_ref = _q.inverse()
	_have_ref = true
