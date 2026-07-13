// YCbCr(separate Y + CbCr planes) -> RGB for the XREAL glasses RGB camera on a 3D surface.
//
// Spatial port of ~/dev/godot-camerafeed-demo/ycbcr_to_rgb.gdshader (the mode==1 / FEED_YCBCR_SEP
// path). The two samplers are CameraTextures bound to an XrealCameraFeed (Y -> FEED_Y_IMAGE r8,
// CbCr -> FEED_CBCR_IMAGE rg8); the feed is fed by set_ycbcr_images. Unshaded so the camera image
// shows as-is regardless of scene lighting.
shader_type spatial;
render_mode unshaded, cull_disabled;

uniform sampler2D y_texture : filter_linear;
uniform sampler2D cbcr_texture : filter_linear;
// 0 = full range (0-255), 1 = video/limited range (16-235 / 16-240). Android is typically video.
uniform int color_range : hint_range(0, 1) = 1;
// The camera Y plane is top-down vs Godot's texture V; flip for an upright image.
uniform bool flip_v = true;

void fragment() {
	vec2 uv = UV;
	if (flip_v) {
		uv.y = 1.0 - uv.y;
	}
	float y = texture(y_texture, uv).r;
	vec2 cbcr = texture(cbcr_texture, uv).rg;

	vec3 ycbcr;
	if (color_range == 1) {
		// Video range (ITU-R BT.601/709): Y (16..235)/219, CbCr (16..240)/224 - 0.5.
		ycbcr.r = (y - 16.0 / 255.0) * (255.0 / 219.0);
		ycbcr.gb = (cbcr - 16.0 / 255.0) * (255.0 / 224.0) - vec2(0.5);
	} else {
		ycbcr.r = y;
		ycbcr.gb = cbcr - vec2(0.5);
	}

	// BT.709 YCbCr -> RGB.
	vec3 rgb = mat3(
			vec3(1.00000, 1.00000, 1.00000),
			vec3(0.00000, -0.18732, 1.85560),
			vec3(1.57481, -0.46813, 0.00000)) * ycbcr;

	ALBEDO = clamp(rgb, vec3(0.0), vec3(1.0));
}
