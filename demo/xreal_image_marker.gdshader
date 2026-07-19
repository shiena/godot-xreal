// Tracked-image marker with distinct front/back colours plus edge orientation cues, for
// verifying the tracked-image pose axes:
//   - facing:            front (+Z-normal) face green, back red (FRONT_FACING)
//   - in-plane rotation: white stripe = the quad's local TOP edge (UV v≈0), yellow stripe = its
//     local RIGHT edge as seen from the front (UV u≈1). Compare against the physical image: the
//     white stripe should sit on the image's top and the yellow on its right — any 90°-step
//     in-plane offset (or a mirror: yellow on the left while white is on top) is instantly
//     readable, which a plain quad can't show at all.
// Assigned to the demo's XrealImageTracking instance through its `marker_material` override
// (demo/main.tscn); the component sets the `tracking` uniform per marker — solid while tracking,
// fainter when limited/lost.
shader_type spatial;
render_mode unshaded, cull_disabled;

uniform vec4 front_color : source_color = vec4(0.3, 1.0, 0.5, 1.0);
uniform vec4 back_color : source_color = vec4(1.0, 0.35, 0.3, 1.0);
uniform vec4 top_color : source_color = vec4(1.0, 1.0, 1.0, 1.0);
uniform vec4 right_color : source_color = vec4(1.0, 0.9, 0.2, 1.0);
uniform bool tracking = true;

void fragment() {
	vec4 c = FRONT_FACING ? front_color : back_color;
	if (UV.y < 0.08) {
		c = top_color;
	} else if (UV.x > 0.92) {
		c = right_color;
	}
	ALBEDO = c.rgb;
	ALPHA = c.a * (tracking ? 0.5 : 0.25);
}
