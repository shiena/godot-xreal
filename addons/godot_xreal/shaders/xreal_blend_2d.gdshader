// Frame blending (mixed-reality composite) — the Godot analog of the SDK's FrameBlender: draws the
// rendered AR scene (with alpha) over the XREAL RGB camera YCbCr feed. Used by the addon's
// blend-capture and streaming feature components (features/xreal_blend_capture.gd / xreal_stream.gd).
shader_type canvas_item;

uniform sampler2D y_texture : filter_linear;    // camera luma (R8)
uniform sampler2D cbcr_texture : filter_linear; // camera chroma (RG8)
uniform sampler2D ar_texture : filter_linear;   // rendered AR SubViewport (RGBA, transparent bg)

// Blend Mode, matching the SDK's VideoCapture: 0 = Blend (AR over the camera image), 1 = RGB Only
// (camera image alone, AR excluded), 2 = Virtual Only (AR alone, over black or the green key).
uniform int blend_mode : hint_range(0, 2) = 0;
// Replace the real-world background with a chroma key. Meaningless for RGB Only, which has no AR
// to key against, so it is ignored there.
uniform bool green_background = false;
uniform vec3 green_key : source_color = vec3(0.0, 1.0, 0.0);

void fragment() {
	float y = texture(y_texture, UV).r;
	vec2 ch = texture(cbcr_texture, UV).rg;
	float u = ch.r; // Cb
	float v = ch.g; // Cr
	float r = y + 1.40220 * v - 0.70110;
	float g = y - 0.34560 * u - 0.71450 * v + 0.53005;
	float b = y + 1.77100 * u - 0.88550;
	vec3 cam = clamp(vec3(b, g, r), vec3(0.0), vec3(1.0)); // SDK B/G/R order

	if (blend_mode == 1) {
		COLOR = vec4(cam, 1.0);  // RGB Only — the AR sampler is not read at all
	} else {
		vec4 ar = texture(ar_texture, UV);  // AR content (premultiply-free, straight alpha)
		// Blend keeps the camera behind the holograms; Virtual Only drops it to black. Either way the
		// key overrides, which is what makes "Virtual Only + green" the chroma-key configuration.
		vec3 bg = (blend_mode == 0) ? cam : vec3(0.0);
		if (green_background) {
			bg = green_key;
		}
		COLOR = vec4(mix(bg, ar.rgb, ar.a), 1.0);
	}
}
