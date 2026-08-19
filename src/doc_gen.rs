//! Documentation generator (test-driven codegen; runs on the host, not on device).
//!
//! `register-docs` makes gdext assemble Godot class-reference XML from the `///` doc comments on
//! each `#[derive(GodotClass)]` and `#[godot_api]` item, correct for the registered GDScript API.
//! [`godot::docs::__gather_xml_docs`] returns those XML strings without needing a live engine, so we
//! run it from a `#[cfg(test)]` entry point and turn it into two committed artifacts, both #included
//! by the desktop dummy (`dummy/gdext_dummy.c`) so the editor F1 help shows the full API:
//!
//!  - `dummy/stub_docs.inc` holds every class's XML embedded as C string literals, which carry the
//!    descriptions, with Rust type names such as `i64`, `GString`, `VarDictionary`, `()` and
//!    `Option<Gd<T>>` mapped to their GDScript equivalents `int`, `String`, `Dictionary`, `void`
//!    and `T`.
//!  - `dummy/stub_members.inc` holds data tables of every registered method, signal and constant
//!    with its signature, mapping the GDScript type to a `GDExtensionVariantType`. The dummy
//!    registers the members from this, since F1 needs them in ClassDB to list them, and loads the
//!    XML from the former for the descriptions.
//!
//! Run it as a generation step, which writes the files, or as a CI sync check, which fails when
//! they drifted:
//! ```text
//! XREAL_DOC_GEN=write cargo test --lib doc_gen -- --nocapture
//! XREAL_DOC_GEN=check cargo test --lib doc_gen -- --nocapture
//! ```
//! The wrapper scripts/gen_docs.{ps1,sh} set the env var, and the release workflow commits the
//! output.

#![cfg(test)]

use std::path::PathBuf;

/// Map a Rust type name, as gdext emits it into the doc XML, to its GDScript type name. It handles
/// the `Option<T>` and `Gd<T>` wrappers and the primitive and builtin renames, and an unknown
/// builtin such as Vector3, Transform3D, a Packed* or ImageTexture passes through unchanged.
fn map_type(raw: &str) -> String {
    // The attribute value is XML-escaped, as `&lt;` and `&gt;`, and gdext pretty-prints generics with
    // spaces, as in "Option < Gd < ImageTexture >>". Normalise both away first.
    let unescaped = raw
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    let compact: String = unescaped.split_whitespace().collect();
    map_inner(&compact)
}

fn map_inner(s: &str) -> String {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix("Option<").and_then(|x| x.strip_suffix('>')) {
        return map_inner(inner);
    }
    if let Some(inner) = s.strip_prefix("Gd<").and_then(|x| x.strip_suffix('>')) {
        return map_inner(inner);
    }
    match s {
        "()" => "void",
        "bool" => "bool",
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize" | "isize" => "int",
        "f32" | "f64" => "float",
        "GString" | "String" | "StringName" => "String",
        "VarDictionary" | "Dictionary" => "Dictionary",
        "VarArray" | "Array" => "Array",
        other => other,
    }
    .to_string()
}

/// Rewrite every `type="…"` attribute value in the XML through [`map_type`]. It applies to both
/// `<return type=…>` and `<param … type=…>`.
fn remap_types(xml: &str) -> String {
    const MARK: &str = "type=\"";
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(pos) = rest.find(MARK) {
        let (head, tail) = rest.split_at(pos + MARK.len());
        out.push_str(head);
        let end = tail.find('"').expect("unterminated type attribute");
        out.push_str(&map_type(&tail[..end]));
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// Collapse the whitespace and `as`-cast noise in computed `#[constant]` values, which gdext emits
/// as raw token streams such as `crate :: ffi :: hmd_feature :: RGB_CAMERA as i64`. Plain integer
/// literals are left untouched.
fn clean_constant_values(xml: &str) -> String {
    const MARK: &str = "<constant name=\"";
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(pos) = rest.find(MARK) {
        let (head, tail) = rest.split_at(pos);
        out.push_str(head);
        // Within this <constant …> opening tag, clean the value="…" attribute.
        let tag_end = tail.find('>').expect("unterminated constant tag");
        let (tag, after) = tail.split_at(tag_end);
        out.push_str(&clean_value_attr(tag));
        rest = after;
    }
    out.push_str(rest);
    out
}

fn clean_value_attr(tag: &str) -> String {
    const MARK: &str = "value=\"";
    let Some(pos) = tag.find(MARK) else {
        return tag.to_string();
    };
    let (head, tail) = tag.split_at(pos + MARK.len());
    let end = tail.find('"').expect("unterminated value attribute");
    let raw = &tail[..end];
    // A computed `#[constant]`, such as `crate :: ffi :: hmd_feature :: RGB_CAMERA as i64`, reaches
    // the doc as its Rust token stream, so resolve those to the actual integer through the ffi source
    // of truth. Plain integer literals, and anything unrecognised, fall back to a light cleanup.
    let resolved = attr(tag, "name")
        .and_then(|n| constant_value_overrides().get(n.as_str()).copied())
        .map(|v| v.to_string())
        .unwrap_or_else(|| {
            raw.split(" as ")
                .next()
                .unwrap_or(raw)
                .replace(" :: ", "::")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        });
    format!("{head}{resolved}{}", &tail[end..])
}

/// Actual integer values for the computed `#[constant]`s, keyed by constant name. They are sourced
/// from the `ffi` definitions so they cannot drift, and a new computed constant missing here simply
/// falls back to the cleaned Rust path, which stays visible.
fn constant_value_overrides() -> std::collections::HashMap<&'static str, i64> {
    use crate::depth_mesh::semantic_label as msl;
    use crate::ffi::{anchor_quality as aq, hmd_feature as hf, plane_detection_mode as pdm};
    std::collections::HashMap::from([
        ("FEATURE_RGB_CAMERA", hf::RGB_CAMERA as i64),
        ("FEATURE_WEARING_STATUS", hf::WEARING_STATUS as i64),
        ("FEATURE_CONTROLLER", hf::CONTROLLER as i64),
        (
            "FEATURE_HEAD_TRACKING_ROTATION",
            hf::HEAD_TRACKING_ROTATION as i64,
        ),
        (
            "FEATURE_HEAD_TRACKING_POSITION",
            hf::HEAD_TRACKING_POSITION as i64,
        ),
        ("PLANE_NONE", pdm::NONE as i64),
        ("PLANE_HORIZONTAL", pdm::HORIZONTAL as i64),
        ("PLANE_VERTICAL", pdm::VERTICAL as i64),
        ("PLANE_BOTH", pdm::BOTH as i64),
        ("ANCHOR_QUALITY_INSUFFICIENT", aq::INSUFFICIENT as i64),
        ("ANCHOR_QUALITY_SUFFICIENT", aq::SUFFICIENT as i64),
        ("ANCHOR_QUALITY_GOOD", aq::GOOD as i64),
        ("MESH_LABEL_BACKGROUND", msl::BACKGROUND as i64),
        ("MESH_LABEL_WALL", msl::WALL as i64),
        ("MESH_LABEL_BUILDING", msl::BUILDING as i64),
        ("MESH_LABEL_FLOOR", msl::FLOOR as i64),
        ("MESH_LABEL_CEILING", msl::CEILING as i64),
        ("MESH_LABEL_HIGHWAY", msl::HIGHWAY as i64),
        ("MESH_LABEL_SIDEWALK", msl::SIDEWALK as i64),
        ("MESH_LABEL_GRASS", msl::GRASS as i64),
        ("MESH_LABEL_DOOR", msl::DOOR as i64),
        ("MESH_LABEL_TABLE", msl::TABLE as i64),
    ])
}

/// Read the value of a `key="..."` attribute out of an opening-tag fragment.
pub(crate) fn attr(tag: &str, key: &str) -> Option<String> {
    let mark = format!("{key}=\"");
    let start = tag.find(&mark)? + mark.len();
    let end = tag[start..].find('"')?;
    Some(tag[start..start + end].to_string())
}

/// Full cleanup of one class's gdext XML: Rust type names become GDScript ones, computed constants
/// are resolved, and runs of blank lines collapse. It is embedded verbatim into stub_docs.inc for
/// the F1 descriptions.
fn clean_xml(raw: &str) -> String {
    let x = remap_types(raw);
    let x = clean_constant_values(&x);
    // Trim runs of blank lines gdext leaves between empty sections.
    let mut lines: Vec<&str> = Vec::new();
    let mut prev_blank = false;
    for line in x.lines() {
        let blank = line.trim().is_empty();
        if blank && prev_blank {
            continue;
        }
        lines.push(line);
        prev_blank = blank;
    }
    let mut s = lines.join("\n");
    s.push('\n');
    s
}

fn class_name(xml: &str) -> String {
    let marker = "<class name=\"";
    let start = xml.find(marker).expect("no <class name=") + marker.len();
    let end = xml[start..].find('"').unwrap();
    xml[start..start + end].to_string()
}

/// Embed one XML document as adjacent C string literals, one per source line, NUL-safe and with
/// only `"` and `\` escaped. UTF-8 bytes pass through, since the dummy is compiled as UTF-8.
fn c_string_literal(xml: &str) -> String {
    let mut s = String::new();
    for line in xml.lines() {
        s.push('"');
        for ch in line.chars() {
            match ch {
                '"' => s.push_str("\\\""),
                '\\' => s.push_str("\\\\"),
                _ => s.push(ch),
            }
        }
        s.push_str("\\n\"\n");
    }
    s
}

fn render_stub_docs_inc(classes: &[(String, String)]) -> String {
    let mut out = String::new();
    out.push_str(
        "/* Generated by scripts/gen_docs.ps1 (Windows) / gen_docs.sh (mac/Linux) from the `///`\n\
         * doc comments in src/ via gdext's `register-docs` (godot::docs::__gather_xml_docs). DO NOT\n\
         * EDIT. One class-reference XML per registered class; the desktop dummy loads them into the\n\
         * editor help database so F1 shows each class's description and, paired with the member\n\
         * registration in stub_members.inc, its methods, signals and constants. Releases commit it. */\n",
    );
    out.push_str("static const char *const stub_docs[] = {\n");
    for (_name, xml) in classes {
        out.push_str(&c_string_literal(xml));
        out.push_str(",\n");
    }
    out.push_str("};\n");
    out.push_str("enum { STUB_DOC_COUNT = sizeof(stub_docs) / sizeof(stub_docs[0]) };\n");
    out
}

// ---- Member registration for the desktop dummy (dummy/stub_members.inc) ----
//
// The editor F1 help only surfaces a method, signal or constant that is *registered in ClassDB*,
// and the loaded XML then supplies its description, matched by name. The desktop dummy's
// placeholders are otherwise empty, so we emit data tables of every registered member, with the
// GDScript type mapped to its `GDExtensionVariantType`, and a small driver in gdext_dummy.c walks
// them and calls classdb_register_extension_class_{method,signal,integer_constant}. The signatures
// come from here and the descriptions from stub_docs.inc. #[export] properties are omitted: they
// show in the inspector on device, and registering them here would need setter and getter
// accessors.

struct MArg {
    /// `GDExtensionVariantType` value.
    ty: i32,
    /// Object class name, such as `ImageTexture`, when `ty` is OBJECT (24), and empty otherwise.
    class: String,
    name: String,
}
struct MMethod {
    name: String,
    ret: i32,
    ret_class: String,
    args: Vec<MArg>,
}
struct MSignal {
    name: String,
    args: Vec<MArg>,
}
struct MConst {
    name: String,
    value: i64,
}
struct ClassMembers {
    class: String,
    methods: Vec<MMethod>,
    signals: Vec<MSignal>,
    consts: Vec<MConst>,
}

/// Map a GDScript type name to a `GDExtensionVariantType` value plus an object class name, or "".
/// An unknown name is treated as an object class, OBJECT (24), so any `Gd<T>` return surfaces as
/// its class.
fn variant_type(gd: &str) -> (i32, String) {
    let v = match gd {
        "void" => 0,
        "bool" => 1,
        "int" => 2,
        "float" => 3,
        "String" => 4,
        "Vector2" => 5,
        "Vector3" => 9,
        "Transform3D" => 18,
        "StringName" => 21,
        "Dictionary" => 27,
        "Array" => 28,
        "PackedByteArray" => 29,
        "PackedInt32Array" => 30,
        "PackedFloat32Array" => 32,
        "PackedStringArray" => 34,
        "PackedVector2Array" => 35,
        "PackedVector3Array" => 36,
        _ => return (24, gd.to_string()),
    };
    (v, String::new())
}

/// Collect each `<tag …>…</tag>` block, used for methods, signals and constants.
pub(crate) fn blocks<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let open_sp = format!("<{tag} ");
    let open_gt = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = xml;
    loop {
        let a = rest.find(&open_sp);
        let b = rest.find(&open_gt);
        let start = match (a, b) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => break,
        };
        let after = &rest[start..];
        let Some(end) = after.find(&close) else { break };
        out.push(&after[..end + close.len()]);
        rest = &after[end + close.len()..];
    }
    out
}

/// Parse the `<param … name="n" type="T" />` entries inside a method/signal block, in order.
fn params(block: &str) -> Vec<MArg> {
    let mut out = Vec::new();
    let mut rest = block;
    while let Some(pos) = rest.find("<param ") {
        let after = &rest[pos..];
        let end = after.find('>').unwrap_or(after.len());
        let tag = &after[..end];
        let name = attr(tag, "name").unwrap_or_default();
        let (ty, class) = variant_type(&attr(tag, "type").unwrap_or_default());
        out.push(MArg { ty, class, name });
        rest = &after[end..];
    }
    out
}

fn extract_members(class: &str, xml: &str) -> ClassMembers {
    let methods = blocks(xml, "method")
        .into_iter()
        .map(|b| {
            let name = attr(b, "name").unwrap_or_default();
            // The first `type="…"` in the block is the <return> type.
            let ret_str = b
                .find("<return")
                .and_then(|p| attr(&b[p..], "type"))
                .unwrap_or_else(|| "void".into());
            let (ret, ret_class) = variant_type(&ret_str);
            MMethod {
                name,
                ret,
                ret_class,
                args: params(b),
            }
        })
        .collect();
    let signals = blocks(xml, "signal")
        .into_iter()
        .map(|b| MSignal {
            name: attr(b, "name").unwrap_or_default(),
            args: params(b),
        })
        .collect();
    let consts = blocks(xml, "constant")
        .into_iter()
        .filter_map(|b| {
            let name = attr(b, "name")?;
            let value = attr(b, "value")?.parse::<i64>().ok()?;
            Some(MConst { name, value })
        })
        .collect();
    ClassMembers {
        class: class.to_string(),
        methods,
        signals,
        consts,
    }
}

fn render_stub_members_inc(all: &[ClassMembers]) -> String {
    let ident = |s: &str| s.replace(|c: char| !c.is_ascii_alphanumeric(), "_");
    let mut out = String::new();
    out.push_str(
        "/* Generated by scripts/gen_docs.{ps1,sh} from the `///`-documented GDScript API via gdext's\n\
         * `register-docs`. DO NOT EDIT. Data tables of every registered method, signal and constant\n\
         * (GDScript type -> GDExtensionVariantType); dummy/gdext_dummy.c walks them so the editor F1\n\
         * help shows the full members. Descriptions come from stub_docs.inc (matched by name). */\n\n",
    );

    // Upper bound on the driver's string_name_new calls, which come from a static pool. A method
    // needs its name, the return name, the return class and 2 per argument; a signal needs its name
    // and 2 per argument; a constant needs its name plus one shared empty enum name per class. It is
    // over-provisioned on purpose, since a void* slot is cheap.
    let mut pool = 8usize;
    let emit_args = |out: &mut String, prefix: &str, args: &[MArg]| {
        if args.is_empty() {
            return;
        }
        out.push_str(&format!("static const StubArg {prefix}[] = {{"));
        for a in args {
            out.push_str(&format!("{{{},\"{}\",\"{}\"}},", a.ty, a.class, a.name));
        }
        out.push_str("};\n");
    };

    for c in all {
        let cid = ident(&c.class);
        for m in &c.methods {
            pool += 4 + 2 * m.args.len();
            emit_args(&mut out, &format!("_a_{cid}_{}", ident(&m.name)), &m.args);
        }
        if !c.methods.is_empty() {
            out.push_str(&format!("static const StubMethod _m_{cid}[] = {{\n"));
            for m in &c.methods {
                let args_ref = if m.args.is_empty() {
                    "0".to_string()
                } else {
                    format!("_a_{cid}_{}", ident(&m.name))
                };
                out.push_str(&format!(
                    "  {{\"{}\",{},\"{}\",{},{}}},\n",
                    m.name,
                    m.ret,
                    m.ret_class,
                    m.args.len(),
                    args_ref
                ));
            }
            out.push_str("};\n");
        }
        for s in &c.signals {
            pool += 2 + 2 * s.args.len();
            emit_args(&mut out, &format!("_sa_{cid}_{}", ident(&s.name)), &s.args);
        }
        if !c.signals.is_empty() {
            out.push_str(&format!("static const StubSignal _s_{cid}[] = {{\n"));
            for s in &c.signals {
                let args_ref = if s.args.is_empty() {
                    "0".to_string()
                } else {
                    format!("_sa_{cid}_{}", ident(&s.name))
                };
                out.push_str(&format!(
                    "  {{\"{}\",{},{}}},\n",
                    s.name,
                    s.args.len(),
                    args_ref
                ));
            }
            out.push_str("};\n");
        }
        if !c.consts.is_empty() {
            pool += c.consts.len() + 2;
            out.push_str(&format!("static const StubConst _c_{cid}[] = {{\n"));
            for k in &c.consts {
                out.push_str(&format!("  {{\"{}\",{}}},\n", k.name, k.value));
            }
            out.push_str("};\n");
        }
    }

    out.push_str("static const StubMembers stub_members[] = {\n");
    for c in all {
        let cid = ident(&c.class);
        let m = if c.methods.is_empty() {
            "0,0".into()
        } else {
            format!("_m_{cid},{}", c.methods.len())
        };
        let s = if c.signals.is_empty() {
            "0,0".into()
        } else {
            format!("_s_{cid},{}", c.signals.len())
        };
        let k = if c.consts.is_empty() {
            "0,0".into()
        } else {
            format!("_c_{cid},{}", c.consts.len())
        };
        out.push_str(&format!("  {{\"{}\",{m},{s},{k}}},\n", c.class));
    }
    out.push_str("};\n");
    out.push_str("enum { STUB_MEMBERS_COUNT = sizeof(stub_members) / sizeof(stub_members[0]) };\n");
    out.push_str(&format!("#define STUB_SN_POOL {pool}\n"));
    out
}

pub(crate) fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every registered class's cleaned reference XML, sorted by class name so the output is
/// deterministic. It is shared with [`crate::api_docs`], which renders the same XML as the Markdown
/// class reference.
pub(crate) fn cleaned_classes() -> Vec<(String, String)> {
    let mut classes: Vec<(String, String)> = godot::docs::__gather_xml_docs()
        .map(|raw| {
            let xml = clean_xml(&raw);
            (class_name(&xml), xml)
        })
        .collect();
    classes.sort_by(|a, b| a.0.cmp(&b.0));
    classes
}

/// The generated desktop-dummy doc artifacts, both #included by dummy/gdext_dummy.c.
struct Generated {
    /// dummy/stub_docs.inc: every class's XML embedded as C literals, for the F1 descriptions.
    docs_inc: String,
    /// dummy/stub_members.inc: every class's registered members, for the F1 signatures.
    members_inc: String,
}

/// Gather and clean every class's XML, sorted by class name for deterministic output, then render
/// the two dummy `.inc` files that drive the editor F1 help.
fn generate() -> Generated {
    let classes = cleaned_classes();

    // The dummy registers all classes, so every one gets its XML loaded for the descriptions and its
    // members registered for the signatures, both feeding the editor F1 help.
    let members: Vec<ClassMembers> = classes
        .iter()
        .map(|(name, xml)| extract_members(name, xml))
        .collect();

    Generated {
        docs_inc: render_stub_docs_inc(&classes),
        members_inc: render_stub_members_inc(&members),
    }
}

#[test]
fn doc_gen() {
    let Some(mode) = std::env::var_os("XREAL_DOC_GEN") else {
        return; // inert during normal `cargo test`
    };
    let mode = mode.to_string_lossy().to_string();
    let root = manifest_dir();
    let docs_inc_path = root.join("dummy/stub_docs.inc");
    let members_inc_path = root.join("dummy/stub_members.inc");

    let gen = generate();
    let artifacts = [
        (&docs_inc_path, &gen.docs_inc),
        (&members_inc_path, &gen.members_inc),
    ];

    match mode.as_str() {
        "write" => {
            for (path, content) in artifacts {
                std::fs::write(path, content).expect("write doc artifact");
            }
            eprintln!("[doc_gen] wrote stub_docs.inc + stub_members.inc");
        }
        "check" => {
            let drift: Vec<String> = artifacts
                .iter()
                .filter(|(path, want)| std::fs::read_to_string(path).unwrap_or_default() != ***want)
                .map(|(path, _)| path.display().to_string())
                .collect();
            assert!(
                drift.is_empty(),
                "doc artifacts out of sync with the `///` doc comments: run \
                 scripts/gen_docs and commit:\n  {}",
                drift.join("\n  ")
            );
            eprintln!("[doc_gen] doc artifacts in sync");
        }
        other => panic!("XREAL_DOC_GEN must be 'write' or 'check', got {other:?}"),
    }
}
