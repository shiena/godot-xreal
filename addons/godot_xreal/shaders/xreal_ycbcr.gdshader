// XREAL glasses RGB camera YCbCr -> RGB on a 3D surface. Conversion ported from the XREAL SDK's
// own sample shader (Samples~/Camera Features/RGBCameraAndCapture/Materials/YUVTransRGB.shader):
// full-range BT.601-style coefficients; it outputs B/G/R (R<->B swapped) then GammaToLinearSpace.
// y_texture = R8 luma (.r = Y); cbcr_texture = RG8 chroma (.r = U/Cb, .g = V/Cr) — plain ImageTextures
// the XrealCameraFeed keeps updated. The upright orientation and the R/B swap are device-calibrated
// constants (verified on XREAL One Pro), so they are baked in rather than exposed as uniforms.
shader_type spatial;
render_mode unshaded, cull_disabled;

uniform sampler2D y_texture : filter_linear;
uniform sampler2D cbcr_texture : filter_linear;

vec3 srgb_to_linear(vec3 c) {
	return mix(c / 12.92, pow((c + 0.055) / 1.055, vec3(2.4)), step(vec3(0.04045), c));
}

void fragment() {
	float y = texture(y_texture, UV).r;
	vec2 ch = texture(cbcr_texture, UV).rg;
	float u = ch.r; // Cb
	float v = ch.g; // Cr

	// XREAL YUVTransRGB (full range): r = Y + 1.4022(V-0.5), etc.
	float r = y + 1.40220 * v - 0.70110;
	float g = y - 0.34560 * u - 0.71450 * v + 0.53005;
	float b = y + 1.77100 * u - 0.88550;

	// SDK output order is B/G/R (R<->B swapped). Godot re-encodes sRGB at output, so pre-linearize
	// (the SDK's GammaToLinearSpace) to avoid double-encoding / blown highlights.
	vec3 rgb = clamp(vec3(b, g, r), vec3(0.0), vec3(1.0));
	ALBEDO = srgb_to_linear(rgb);
}
