/* Desktop stand-in for the Android-only godot_xreal GDExtension.
 *
 * Godot prints "No GDExtension library found for current OS and architecture" on every
 * editor start when a .gdextension has no [libraries] entry matching the host platform —
 * there is no way to declare an extension platform-only. This stub gives the desktop
 * editor a real library to load: its entry point accepts initialization and registers
 * nothing, so ClassDB.class_exists(&"XrealSystem") stays false and demo/main.gd takes
 * its no-extension path, exactly as on a device without the .so.
 *
 * Deliberately freestanding (no libc) so every desktop target cross-compiles from any
 * host with clang + lld alone — see scripts/build_dummy_libs.ps1 / .sh. The binaries are
 * not committed: build them once after cloning (rebuild only if the entry_symbol or this
 * ABI ever changes).
 *
 * ABI: the GDExtensionInitialization struct and entry signature from Godot's
 * gdextension_interface.h (stable since 4.1).
 */

#include <stdint.h>

#if defined(_WIN32)
#define GDE_EXPORT __declspec(dllexport)
#else
#define GDE_EXPORT __attribute__((visibility("default")))
#endif

typedef int32_t GDExtensionInitializationLevel; /* enum; 2 = GDEXTENSION_INITIALIZATION_SCENE */

typedef struct {
	GDExtensionInitializationLevel minimum_initialization_level;
	void *userdata;
	void (*initialize)(void *userdata, GDExtensionInitializationLevel p_level);
	void (*deinitialize)(void *userdata, GDExtensionInitializationLevel p_level);
} GDExtensionInitialization;

static void gdext_dummy_noop(void *userdata, GDExtensionInitializationLevel p_level) {
	(void)userdata;
	(void)p_level;
}

/* Same entry_symbol as the real Rust library (godot_xreal.gdextension). */
GDE_EXPORT uint8_t gdext_rust_init(void *p_get_proc_address, void *p_library,
		GDExtensionInitialization *r_initialization) {
	(void)p_get_proc_address;
	(void)p_library;
	r_initialization->minimum_initialization_level = 2;
	r_initialization->userdata = 0;
	r_initialization->initialize = gdext_dummy_noop;
	r_initialization->deinitialize = gdext_dummy_noop;
	return 1;
}
