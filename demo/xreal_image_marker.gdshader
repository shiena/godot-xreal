// Tracked-image marker with distinct front/back colours, for verifying the tracked-image pose
// orientation: the front (+Z-normal) face draws green, the back red, picked via FRONT_FACING —
// so which way the quad faces is obvious at a glance. Assigned to the demo's XrealImageTracking
// instance through its `marker_material` override (demo/main.tscn); the component sets the
// `tracking` uniform per marker — solid while tracking, fainter when limited/lost.
shader_type spatial;
render_mode unshaded, cull_disabled;

uniform vec4 front_color : source_color = vec4(0.3, 1.0, 0.5, 1.0);
uniform vec4 back_color : source_color = vec4(1.0, 0.35, 0.3, 1.0);
uniform bool tracking = true;

void fragment() {
	vec4 c = FRONT_FACING ? front_color : back_color;
	ALBEDO = c.rgb;
	ALPHA = c.a * (tracking ? 0.5 : 0.25);
}
