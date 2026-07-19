// Frame blending (mixed-reality composite) — the Godot analog of the SDK's FrameBlender: draws the
// rendered AR scene (with alpha) over the XREAL RGB camera YCbCr feed. Used by the addon's
// blend-capture and streaming feature components (features/xreal_blend_capture.gd / xreal_stream.gd).
shader_type canvas_item;

uniform sampler2D y_texture : filter_linear;    // camera luma (R8)
uniform sampler2D cbcr_texture : filter_linear; // camera chroma (RG8)
uniform sampler2D ar_texture : filter_linear;   // rendered AR SubViewport (RGBA, transparent bg)

void fragment() {
	float y = texture(y_texture, UV).r;
	vec2 ch = texture(cbcr_texture, UV).rg;
	float u = ch.r; // Cb
	float v = ch.g; // Cr
	float r = y + 1.40220 * v - 0.70110;
	float g = y - 0.34560 * u - 0.71450 * v + 0.53005;
	float b = y + 1.77100 * u - 0.88550;
	vec3 cam = clamp(vec3(b, g, r), vec3(0.0), vec3(1.0)); // SDK B/G/R order
	vec4 ar = texture(ar_texture, UV);                     // AR content (premultiply-free, straight alpha)
	COLOR = vec4(mix(cam, ar.rgb, ar.a), 1.0);             // AR over camera
}
