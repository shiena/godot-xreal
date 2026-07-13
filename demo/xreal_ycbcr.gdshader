// XREAL glasses RGB camera YCbCr -> RGB on a 3D surface. Conversion ported from the XREAL SDK's
// own sample shader (Samples~/Camera Features/RGBCameraAndCapture/Materials/YUVTransRGB.shader):
// full-range BT.601-style coefficients. y_texture = R8 luma (.r = Y); cbcr_texture = RG8 chroma
// (.r = U/Cb, .g = V/Cr) — both plain ImageTextures the XrealCameraFeed keeps updated.
shader_type spatial;
render_mode unshaded, cull_disabled;

uniform sampler2D y_texture : filter_linear;
uniform sampler2D cbcr_texture : filter_linear;
// The camera Y plane is top-down vs Godot's texture V; flip for an upright image.
uniform bool flip_v = true;
// The SDK sample swaps R<->B on output; expose it so we can match on device.
uniform bool swap_rb = false;
// The YUV->RGB result is gamma/sRGB-encoded (like a camera). Godot treats ALBEDO as linear and
// re-applies sRGB at output, so feeding sRGB directly blows out highlights (double-encode). Convert
// sRGB->linear here (the SDK did GammaToLinearSpace for the same reason). Toggle off if the renderer
// is already in an sRGB workflow.
uniform bool to_linear = true;

vec3 srgb_to_linear(vec3 c) {
	return mix(c / 12.92, pow((c + 0.055) / 1.055, vec3(2.4)), step(vec3(0.04045), c));
}

void fragment() {
	vec2 uv = UV;
	if (flip_v) {
		uv.y = 1.0 - uv.y;
	}
	float y = texture(y_texture, uv).r;
	vec2 ch = texture(cbcr_texture, uv).rg;
	float u = ch.r; // Cb
	float v = ch.g; // Cr

	// XREAL YUVTransRGB (full range): r = Y + 1.4022(V-0.5), etc.
	float r = y + 1.40220 * v - 0.70110;
	float g = y - 0.34560 * u - 0.71450 * v + 0.53005;
	float b = y + 1.77100 * u - 0.88550;
	vec3 rgb = clamp(vec3(r, g, b), vec3(0.0), vec3(1.0));
	if (swap_rb) {
		rgb = rgb.bgr;
	}
	if (to_linear) {
		rgb = srgb_to_linear(rgb);
	}
	ALBEDO = rgb;
}
