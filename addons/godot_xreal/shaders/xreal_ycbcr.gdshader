// XREAL glasses RGB camera YCbCr -> RGB on a 3D surface. The conversion is ported from the XREAL
// SDK's own sample shader (Samples~/Camera Features/RGBCameraAndCapture/Materials/
// YUVTransRGB.shader): full-range BT.601-style coefficients, output in B/G/R order (R and B
// swapped), then GammaToLinearSpace. y_texture is the R8 luma (.r = Y) and cbcr_texture the RG8
// chroma (.r = U/Cb, .g = V/Cr), both plain ImageTextures the XrealCameraFeed keeps updated. The
// upright orientation and the R/B swap are device-calibrated constants, verified on the XREAL One
// Pro, so they are baked in rather than exposed as uniforms.
shader_type spatial;
render_mode unshaded, cull_disabled;

uniform sampler2D y_texture : filter_linear;
uniform sampler2D cbcr_texture : filter_linear;
// Compatibility's scene.glsl applies an extra srgb_to_linear to ALBEDO ("Convert colors to
// linear", ~line 2398), which the single srgb_to_linear below was calibrated against. Forward
// Mobile (the RD/Vulkan renderers) does NOT, so the camera came out bright and desaturated there.
// When true, linearize a second time to reproduce Compatibility. camera_preview.gd sets it from
// RenderingServer.get_rendering_device() != null (non-null only on the RD renderers).
uniform bool double_srgb = false;

vec3 srgb_to_linear(vec3 c) {
	return mix(c / 12.92, pow((c + 0.055) / 1.055, vec3(2.4)), step(vec3(0.04045), c));
}

// The SDK hands its camera buffer over bottom-up, Unity's texture origin, while Godot samples
// top-down. Every consumer of these two textures flips V here rather than at upload, because
// camera_feed.rs has three upload routes into them - a GL PBO path, an Image path and a Vulkan
// path - and two of those are deliberately straight copies with no per-row work.
vec2 camera_uv(vec2 uv) {
	return vec2(uv.x, 1.0 - uv.y);
}

void fragment() {
	vec2 cam_uv = camera_uv(UV);
	float y = texture(y_texture, cam_uv).r;
	vec2 ch = texture(cbcr_texture, cam_uv).rg;
	float u = ch.r; // Cb
	float v = ch.g; // Cr

	// XREAL YUVTransRGB (full range): r = Y + 1.4022(V-0.5), etc.
	float r = y + 1.40220 * v - 0.70110;
	float g = y - 0.34560 * u - 0.71450 * v + 0.53005;
	float b = y + 1.77100 * u - 0.88550;

	// SDK output order is B/G/R (R and B swapped). Godot re-encodes sRGB at output, so pre-linearize
	// with the SDK's GammaToLinearSpace to avoid double-encoding and blown highlights.
	vec3 rgb = clamp(vec3(b, g, r), vec3(0.0), vec3(1.0));
	vec3 lin = srgb_to_linear(rgb);
	if (double_srgb) {
		lin = srgb_to_linear(lin);
	}
	ALBEDO = lin;
}
