/* Desktop stand-in for the Android-only godot_xreal GDExtension.
 *
 * Godot prints "No GDExtension library found for current OS and architecture" on every
 * editor start when a .gdextension has no [libraries] entry matching the host platform —
 * there is no way to declare an extension platform-only. This stub gives the desktop
 * editor a real library to load.
 *
 * It registers a placeholder for every real class (stub_classes.inc): the Node-derived ones
 * (XrealHeadTracker / XrealHandTracker / XrealAR) so scenes that place them (demo/main.tscn,
 * addons/godot_xreal/xreal_rig.tscn) open without "missing type" warnings, plus XrealSystem
 * (RefCounted) and XrealCameraFeed (CameraFeed). Each instantiates as its plain base class and
 * does nothing. The demo scripts must NOT treat class presence as "the real extension is here"
 * (these placeholders exist on desktop): they gate the real path on OS.get_name() == "Android".
 *
 * For the editor F1 help it does two things at the EDITOR init level: registers each class's
 * members (stub_members.inc — methods / signals / constants with their signatures, as no-ops) so
 * the members appear at all, and loads each class's reference XML (stub_docs.inc) into the help
 * database so the descriptions (and class briefs) fill in — both generated from the Rust `///`
 * docs by scripts/gen_docs. The same reference lives standalone in addons/godot_xreal/doc_classes/.
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
/* Editor-only (null in a running game / on a Godot that predates 4.3): loads class-reference
 * XML into the editor's help database so the F1 docs work for our placeholder classes. */
static GDExtensionsInterfaceEditorHelpLoadXmlFromUtf8Chars g_editor_help_load_xml;
/* PROTOTYPE (temporary): member-registration procs — testing whether registering the members in
 * ClassDB makes the editor F1 help show them (+ whether the signature comes from the loaded XML). */
static GDExtensionInterfaceClassdbRegisterExtensionClassMethod g_register_method;
static GDExtensionInterfaceClassdbRegisterExtensionClassIntegerConstant g_register_constant;
static GDExtensionInterfaceClassdbRegisterExtensionClassSignal g_register_signal;
static GDExtensionInterfaceStringNewWithLatin1Chars g_string_new;
static GDExtensionInterfaceVariantNewNil g_variant_new_nil;

/* One placeholder class: names + the (pointer-sized, static so never destructed)
 * StringName storage for each. */
typedef struct {
	const char *class_name;
	const char *base_name;
	void *class_sn;
	void *base_sn;
	GDExtensionBool registered;
} StubClass;

/* The Node-derived classes of the real extension. The array is generated from the
 * `#[class(base = ...)]` declarations in the .rs sources under src/ by
 * scripts/gen_stub_classes.ps1 (Windows) / .sh (mac/Linux) — run by the matching
 * build_dummy_libs script; the release workflow commits the regenerated list per
 * release — so the Rust source stays the single source of truth. */
#include "stub_classes.inc"
enum { STUB_CLASS_COUNT = sizeof(stub_classes) / sizeof(stub_classes[0]) };

/* The class-reference documentation, one XML string per class, generated from the Rust
 * `///` doc comments + signatures. Loaded into the editor's help database at the EDITOR
 * init level (see gdext_dummy_initialize) so F1 works on desktop for these classes. */
#include "stub_docs.inc"

/* Registered members of each class (methods / signals / constants with their signatures), so the
 * editor F1 help lists them — descriptions come from stub_docs.inc, matched by name. Both are
 * generated from the Rust `///`-documented GDScript API by scripts/gen_docs. */
typedef struct {
	int type; /* GDExtensionVariantType */
	const char *class_name; /* object class when type == OBJECT (24), else "" */
	const char *name;
} StubArg;
typedef struct {
	const char *name;
	int ret_type; /* GDExtensionVariantType; 0 (NIL) = void / no return */
	const char *ret_class;
	int n_args;
	const StubArg *args;
} StubMethod;
typedef struct {
	const char *name;
	int n_args;
	const StubArg *args;
} StubSignal;
typedef struct {
	const char *name;
	long long value;
} StubConst;
typedef struct {
	const char *class_name;
	const StubMethod *methods;
	int n_methods;
	const StubSignal *sigs;
	int n_sigs;
	const StubConst *consts;
	int n_consts;
} StubMembers;
#include "stub_members.inc"

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

/* freestanding: no libc strcmp. */
static int str_eq(const char *a, const char *b) {
	while (*a && *b) {
		if (*a != *b) {
			return 0;
		}
		a++;
		b++;
	}
	return *a == *b;
}

/* No-op body for the registered placeholder methods — they exist only so the class carries the
 * method in ClassDB (the editor F1 help needs that); nothing ever calls them on desktop. */
static void stub_method_noop(void *method_userdata, GDExtensionClassInstancePtr p_instance,
		const GDExtensionConstVariantPtr *p_args, GDExtensionInt p_argument_count,
		GDExtensionVariantPtr r_return, GDExtensionCallError *r_error) {
	(void)method_userdata;
	(void)p_instance;
	(void)p_args;
	(void)p_argument_count;
	/* Return nil rather than leaving r_return uninitialised, in case a method is ever called (these
	 * placeholders only run on desktop, where the demo gates the extension off — but be safe). */
	if (r_return && g_variant_new_nil) {
		g_variant_new_nil(r_return);
	}
	if (r_error) {
		r_error->error = GDEXTENSION_CALL_OK;
	}
}

/* Persistent StringName storage — Godot may hold the pointers we register, so they must outlive the
 * registration calls (freestanding: a fixed static pool, sized by the generator in stub_members.inc). */
static void *g_sn_pool[STUB_SN_POOL];
static int g_sn_next;
static void *g_empty_string; /* one empty String, reused for every PropertyInfo.hint_string */

static GDExtensionStringNamePtr sn_intern(const char *s) {
	void **slot = &g_sn_pool[g_sn_next++];
	g_string_name_new(slot, s, 1 /* static */);
	return slot;
}

static void fill_prop(GDExtensionPropertyInfo *pi, int type, const char *class_name, const char *name) {
	pi->type = (GDExtensionVariantType)type;
	pi->name = sn_intern(name);
	pi->class_name = sn_intern(class_name ? class_name : "");
	pi->hint = 0;
	pi->hint_string = &g_empty_string; /* GDExtensionStringPtr = pointer to the String storage */
	pi->usage = 6; /* PROPERTY_USAGE_DEFAULT (STORAGE | EDITOR) */
}

/* Register a class's methods / signals / constants so the editor F1 help lists them (their
 * descriptions load from the class XML separately). */
static void register_members(GDExtensionConstStringNamePtr class_sn, const StubMembers *sm) {
	for (int i = 0; i < sm->n_methods; i++) {
		const StubMethod *m = &sm->methods[i];
		GDExtensionPropertyInfo ret_pi;
		GDExtensionPropertyInfo args_pi[16];
		GDExtensionClassMethodArgumentMetadata args_md[16];
		int n = m->n_args < 16 ? m->n_args : 16;
		fill_prop(&ret_pi, m->ret_type, m->ret_class, "");
		for (int a = 0; a < n; a++) {
			fill_prop(&args_pi[a], m->args[a].type, m->args[a].class_name, m->args[a].name);
			args_md[a] = GDEXTENSION_METHOD_ARGUMENT_METADATA_NONE;
		}
		GDExtensionClassMethodInfo mi;
		mi.name = sn_intern(m->name);
		mi.method_userdata = 0;
		mi.call_func = stub_method_noop;
		mi.ptrcall_func = 0;
		mi.method_flags = GDEXTENSION_METHOD_FLAG_NORMAL;
		mi.has_return_value = m->ret_type != 0;
		mi.return_value_info = &ret_pi;
		mi.return_value_metadata = GDEXTENSION_METHOD_ARGUMENT_METADATA_NONE;
		mi.argument_count = (uint32_t)n;
		mi.arguments_info = args_pi;
		mi.arguments_metadata = args_md;
		mi.default_argument_count = 0;
		mi.default_arguments = 0;
		g_register_method(g_library, class_sn, &mi);
	}
	for (int i = 0; i < sm->n_sigs; i++) {
		const StubSignal *s = &sm->sigs[i];
		GDExtensionPropertyInfo args_pi[16];
		int n = s->n_args < 16 ? s->n_args : 16;
		for (int a = 0; a < n; a++) {
			fill_prop(&args_pi[a], s->args[a].type, s->args[a].class_name, s->args[a].name);
		}
		g_register_signal(g_library, class_sn, sn_intern(s->name), n ? args_pi : 0, n);
	}
	if (sm->n_consts > 0) {
		GDExtensionConstStringNamePtr enum_sn = sn_intern("");
		for (int i = 0; i < sm->n_consts; i++) {
			g_register_constant(g_library, class_sn, enum_sn, sn_intern(sm->consts[i].name),
					(GDExtensionInt)sm->consts[i].value, 0);
		}
	}
}

static void gdext_dummy_initialize(void *userdata, GDExtensionInitializationLevel p_level) {
	(void)userdata;
	/* EDITOR level (editor only; never reached in an exported game): load each class's reference
	 * XML so the F1 help fills in the descriptions for the members registered at the SCENE level.
	 * The proc is null on a running game and on pre-4.3 editors — degrade to no docs. */
	if (p_level == GDEXTENSION_INITIALIZATION_EDITOR) {
		if (g_editor_help_load_xml) {
			for (int i = 0; i < STUB_DOC_COUNT; i++) {
				g_editor_help_load_xml(stub_docs[i]);
			}
		}
		return;
	}
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

	/* Register each class's members so the editor F1 help lists them (their descriptions load from
	 * the class XML at the EDITOR level). Needs the member-registration procs + an empty String for
	 * the property-info hint strings; degrade to class-only docs when any is missing. */
	if (g_register_method && g_register_signal && g_register_constant && g_string_new) {
		g_string_new(&g_empty_string, "");
		for (int i = 0; i < STUB_MEMBERS_COUNT; i++) {
			for (int j = 0; j < STUB_CLASS_COUNT; j++) {
				if (str_eq(stub_classes[j].class_name, stub_members[i].class_name)) {
					register_members(&stub_classes[j].class_sn, &stub_members[i]);
					break;
				}
			}
		}
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
	g_editor_help_load_xml = (GDExtensionsInterfaceEditorHelpLoadXmlFromUtf8Chars)p_get_proc_address("editor_help_load_xml_from_utf8_chars");
	g_register_method = (GDExtensionInterfaceClassdbRegisterExtensionClassMethod)p_get_proc_address("classdb_register_extension_class_method");
	g_register_constant = (GDExtensionInterfaceClassdbRegisterExtensionClassIntegerConstant)p_get_proc_address("classdb_register_extension_class_integer_constant");
	g_register_signal = (GDExtensionInterfaceClassdbRegisterExtensionClassSignal)p_get_proc_address("classdb_register_extension_class_signal");
	g_string_new = (GDExtensionInterfaceStringNewWithLatin1Chars)p_get_proc_address("string_new_with_latin1_chars");
	g_variant_new_nil = (GDExtensionInterfaceVariantNewNil)p_get_proc_address("variant_new_nil");
	r_initialization->minimum_initialization_level = GDEXTENSION_INITIALIZATION_SCENE;
	r_initialization->userdata = 0;
	r_initialization->initialize = gdext_dummy_initialize;
	r_initialization->deinitialize = gdext_dummy_deinitialize;
	return 1;
}
