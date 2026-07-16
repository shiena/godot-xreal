// Canvas (2D) twin of xreal_ycbcr.gdshader — used by demo/capture_manager.gd to render the XREAL RGB
// camera YCbCr feed into an offscreen SubViewport for a JPG photo readback (the Godot analog of the
// SDK's XREALPhotoCapture EncodeToJPG). Same full-range BT.601 conversion + B/G/R output order as the
// spatial shader, but writes COLOR directly (canvas is already sRGB, so no srgb_to_linear).
shader_type canvas_item;

uniform sampler2D y_texture : filter_linear;
uniform sampler2D cbcr_texture : filter_linear;

void fragment() {
	float y = texture(y_texture, UV).r;
	vec2 ch = texture(cbcr_texture, UV).rg;
	float u = ch.r; // Cb
	float v = ch.g; // Cr
	float r = y + 1.40220 * v - 0.70110;
	float g = y - 0.34560 * u - 0.71450 * v + 0.53005;
	float b = y + 1.77100 * u - 0.88550;
	// SDK output order is B/G/R (R<->B swapped, device-calibrated).
	COLOR = vec4(clamp(vec3(b, g, r), vec3(0.0), vec3(1.0)), 1.0);
}
