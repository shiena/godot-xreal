/* Desktop stand-in for the Android-only godot_xreal GDExtension.
 *
 * Godot prints "No GDExtension library found for current OS and architecture" on every
 * editor start when a .gdextension has no [libraries] entry matching the host platform —
 * there is no way to declare an extension platform-only. This stub gives the desktop
 * editor a real library to load.
 *
 * It registers empty placeholders for the Node-derived classes (XrealHeadTracker /
 * XrealHandTracker / XrealAR) so scenes that place them (demo/main.tscn,
 * addons/godot_xreal/xreal_rig.tscn) open in the desktop editor without "missing type"
 * warnings — each instantiates as its plain base class and does nothing. XrealSystem and
 * XrealCameraFeed are deliberately NOT registered: scripts gate the real-extension path
 * on ClassDB.class_exists(&"XrealSystem") (demo/main.gd), which must stay false on
 * desktop so they take their no-extension fallback, exactly as on a device without
 * the .so.
 *
 * Deliberately freestanding (no libc) so every desktop target cross-compiles from any
 * host with clang + lld alone — see scripts/build_dummy_libs.ps1 / .sh. The binaries are
 * not committed: build them once after cloning (rebuild only if this file, the
 * entry_symbol, or the registered class list changes).
 *
 * ABI: gdextension_interface.h alongside this file is the verbatim official header,
 * dumped from the pinned editor with `godot --dump-gdextension-interface` (4.7.1-stable).
 * Registration uses classdb_register_extension_class2 / classdb_construct_object: both
 * exist since 4.1/4.2 (deprecated but kept in official builds), the CreationInfo2 layout
 * is frozen, and the v1 construct_object sends NOTIFICATION_POSTINITIALIZE itself — the
 * newer non-deprecated APIs would force this stub to send that notification through a
 * method bind. Every proc is null-checked; on a Godot that dropped these symbols the
 * stub degrades to registering nothing (the pre-placeholder behavior).
 */

#include "gdextension_interface.h"

#if defined(_WIN32)
#define GDE_EXPORT __declspec(dllexport)
#else
#define GDE_EXPORT __attribute__((visibility("default")))
#endif

static GDExtensionClassLibraryPtr g_library;
static GDExtensionInterfaceStringNameNewWithLatin1Chars g_string_name_new;
static GDExtensionInterfaceClassdbRegisterExtensionClass2 g_register_class;
static GDExtensionInterfaceClassdbUnregisterExtensionClass g_unregister_class;
static GDExtensionInterfaceClassdbConstructObject g_construct_object;
static GDExtensionInterfaceObjectSetInstance g_object_set_instance;

/* One placeholder class: names + the (pointer-sized, static so never destructed)
 * StringName storage for each. */
typedef struct {
	const char *class_name;
	const char *base_name;
	void *class_sn;
	void *base_sn;
	GDExtensionBool registered;
} StubClass;

/* The Node-derived classes of the real extension (src/node.rs, src/hand_tracking.rs,
 * src/system.rs::XrealAR). Keep in sync when a new scene-placeable class is added. */
static StubClass stub_classes[] = {
	{ "XrealHeadTracker", "Node3D", 0, 0, 0 },
	{ "XrealHandTracker", "Node", 0, 0, 0 },
	{ "XrealAR", "Node", 0, 0, 0 },
};
enum { STUB_CLASS_COUNT = sizeof(stub_classes) / sizeof(stub_classes[0]) };

/* The instance is stateless — the StubClass record doubles as the (never dereferenced,
 * merely non-null) instance pointer. construct_object (v1) sends
 * NOTIFICATION_POSTINITIALIZE itself. */
static GDExtensionObjectPtr stub_create_instance(void *p_class_userdata) {
	StubClass *cls = (StubClass *)p_class_userdata;
	GDExtensionObjectPtr obj = g_construct_object(&cls->base_sn);
	g_object_set_instance(obj, &cls->class_sn, (GDExtensionClassInstancePtr)cls);
	return obj;
}

static void stub_free_instance(void *p_class_userdata, GDExtensionClassInstancePtr p_instance) {
	(void)p_class_userdata;
	(void)p_instance;
}

/* Hot-reload (reloadable = true): reuse the stateless record as the new instance. */
static GDExtensionClassInstancePtr stub_recreate_instance(void *p_class_userdata, GDExtensionObjectPtr p_object) {
	(void)p_object;
	return (GDExtensionClassInstancePtr)p_class_userdata;
}

static void gdext_dummy_initialize(void *userdata, GDExtensionInitializationLevel p_level) {
	(void)userdata;
	if (p_level != GDEXTENSION_INITIALIZATION_SCENE) {
		return;
	}
	if (!g_string_name_new || !g_register_class || !g_construct_object || !g_object_set_instance) {
		return;
	}
	/* Static, zero-initialized (.bss — no memset in a freestanding build); only the
	 * fields below are ever non-null. register copies it, so one struct serves all. */
	static GDExtensionClassCreationInfo2 info;
	info.is_exposed = 1;
	info.create_instance_func = stub_create_instance;
	info.free_instance_func = stub_free_instance;
	info.recreate_instance_func = stub_recreate_instance;
	for (int i = 0; i < STUB_CLASS_COUNT; i++) {
		StubClass *cls = &stub_classes[i];
		g_string_name_new(&cls->class_sn, cls->class_name, 1 /* static */);
		g_string_name_new(&cls->base_sn, cls->base_name, 1 /* static */);
		info.class_userdata = cls;
		g_register_class(g_library, &cls->class_sn, &cls->base_sn, &info);
		cls->registered = 1;
	}
}

static void gdext_dummy_deinitialize(void *userdata, GDExtensionInitializationLevel p_level) {
	(void)userdata;
	if (p_level != GDEXTENSION_INITIALIZATION_SCENE || !g_unregister_class) {
		return;
	}
	for (int i = STUB_CLASS_COUNT - 1; i >= 0; i--) {
		if (stub_classes[i].registered) {
			g_unregister_class(g_library, &stub_classes[i].class_sn);
			stub_classes[i].registered = 0;
		}
	}
}

/* Same entry_symbol as the real Rust library (godot_xreal.gdextension). */
GDE_EXPORT GDExtensionBool gdext_rust_init(GDExtensionInterfaceGetProcAddress p_get_proc_address,
		GDExtensionClassLibraryPtr p_library, GDExtensionInitialization *r_initialization) {
	g_library = p_library;
	g_string_name_new = (GDExtensionInterfaceStringNameNewWithLatin1Chars)p_get_proc_address("string_name_new_with_latin1_chars");
	g_register_class = (GDExtensionInterfaceClassdbRegisterExtensionClass2)p_get_proc_address("classdb_register_extension_class2");
	g_unregister_class = (GDExtensionInterfaceClassdbUnregisterExtensionClass)p_get_proc_address("classdb_unregister_extension_class");
	g_construct_object = (GDExtensionInterfaceClassdbConstructObject)p_get_proc_address("classdb_construct_object");
	g_object_set_instance = (GDExtensionInterfaceObjectSetInstance)p_get_proc_address("object_set_instance");
	r_initialization->minimum_initialization_level = GDEXTENSION_INITIALIZATION_SCENE;
	r_initialization->userdata = 0;
	r_initialization->initialize = gdext_dummy_initialize;
	r_initialization->deinitialize = gdext_dummy_deinitialize;
	return 1;
}
