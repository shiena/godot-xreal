//! Web-facing class reference generator (test-driven codegen; runs on the host, not on device).
//!
//! Renders one Markdown page per public class into `docs/api/`, out of the *same* Godot
//! class-reference XML on both sides of the addon:
//!
//!  - the native classes — gdext's `register-docs` XML built from the `///` comments, handed over by
//!    [`crate::doc_gen::cleaned_classes`] (no live engine needed).
//!  - the GDScript feature components — the editor's own doctool, which turns the `##` comments into
//!    the very same schema: `godot --headless --doctool <dir> --gdscript-docs res://addons/godot_xreal`.
//!
//! One schema means one renderer, so both halves come out identical in shape and cross-link into each
//! other. The GDScript half needs the editor binary, which CI has not got, so the flow is: run Godot,
//! then run this over its output, and commit the pages — the same policy as the editor F1 artifacts in
//! [`crate::doc_gen`]. `scripts/gen_api_docs.{ps1,sh}` does both steps.
//!
//! ```text
//! XREAL_API_DOCS=write XREAL_GDSCRIPT_XML=<dir> cargo test --lib api_docs -- --nocapture
//! XREAL_API_DOCS=check XREAL_GDSCRIPT_XML=<dir> cargo test --lib api_docs -- --nocapture
//! ```
//!
//! The output is plain CommonMark with explicit `<a id="…">` anchors — no static-site-generator
//! syntax — so it renders on GitHub as-is and any generator (mdBook / MkDocs / VitePress) can consume
//! it unchanged.

#![cfg(test)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::doc_gen::{attr, blocks, cleaned_classes, manifest_dir};

/// Scripts under `addons/godot_xreal/` that are not part of the runtime API and so get no page: the
/// editor-side plugin and its docks (only ever loaded by the editor), and `xreal_gallery.gd` (an
/// orphan superseded by `demo/gallery_helper.gd`). The doctool documents every script it is pointed
/// at, so this is where that policy lives.
const EXCLUDED_SCRIPTS: &[&str] = &[
    "addons/godot_xreal/plugin.gd",
    "addons/godot_xreal/export_plugin.gd",
    "addons/godot_xreal/editor/image_db_dock.gd",
    "addons/godot_xreal/editor/vendor_import_dock.gd",
    "addons/godot_xreal/xreal_gallery.gd",
];

/// Godot's online class reference, for types we do not define ourselves.
fn godot_class_url(name: &str) -> String {
    format!(
        "https://docs.godotengine.org/en/stable/classes/class_{}.html",
        name.to_lowercase()
    )
}

// ---- Model ----------------------------------------------------------------------------------

/// Which half of the addon a class comes from — decides its section in the index and how the page
/// introduces it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Group {
    /// Registered by the GDExtension (Rust).
    Native,
    /// A drop-in feature component scene under `addons/godot_xreal/features/`.
    Feature,
    /// Any other GDScript class in the addon (static helpers).
    Helper,
}

struct Param {
    name: String,
    ty: String,
    default: Option<String>,
}

struct Method {
    name: String,
    ret: String,
    params: Vec<Param>,
    is_static: bool,
    desc: String,
}

struct Signal {
    name: String,
    params: Vec<Param>,
    desc: String,
}

struct Property {
    name: String,
    /// The declared type, or the enum's qualified name when the member is enum-typed.
    ty: String,
    default: Option<String>,
    desc: String,
}

struct Constant {
    name: String,
    value: String,
    /// Set for enum members (`enum ResolutionLevel { … }`), which are rendered grouped.
    enum_name: Option<String>,
    desc: String,
}

struct ClassDoc {
    /// Display name: the registered/global class name, or — for a feature script without a
    /// `class_name` — the PascalCase of its file stem (see the note the page carries).
    name: String,
    /// Output file stem under `docs/api/`.
    slug: String,
    inherits: String,
    group: Group,
    /// Repo-relative script path (GDScript classes only).
    script: Option<String>,
    /// The component's scene, when one sits next to the script.
    scene: Option<String>,
    /// True when the script declares no `class_name`, so [`Self::name`] is ours, not a real type.
    unnamed: bool,
    brief: String,
    description: String,
    properties: Vec<Property>,
    methods: Vec<Method>,
    signals: Vec<Signal>,
    constants: Vec<Constant>,
    /// GDScript descriptions come pre-joined into one line by the doctool and need [`unjoin`].
    from_gdscript: bool,
}

/// Everything cross-linking needs: which classes exist (and their members), plus the engine class
/// names actually used in this API — the only outside names a bare `[Name]` reference links out to.
struct Index {
    classes: BTreeMap<String, Entry>,
    engine: BTreeSet<String>,
}

struct Entry {
    slug: String,
    methods: BTreeSet<String>,
    signals: BTreeSet<String>,
    properties: BTreeSet<String>,
    constants: BTreeSet<String>,
}

impl Index {
    fn anchor(&self, kind: &str, class: &str, name: &str) -> Option<(&str, String)> {
        let e = self.classes.get(class)?;
        let (set, prefix) = match kind {
            "method" => (&e.methods, "method"),
            "signal" => (&e.signals, "signal"),
            "member" => (&e.properties, "property"),
            "constant" => (&e.constants, "constant"),
            _ => return None,
        };
        set.contains(name)
            .then(|| (e.slug.as_str(), format!("{prefix}-{name}")))
    }
}

// ---- XML plumbing ---------------------------------------------------------------------------

/// Decode the XML entities Godot writes (`&amp;` `&lt;` `&#39;` …). Unknown entities are left alone.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos..];
        let decoded = rest.find(';').and_then(|semi| {
            let ent = &rest[1..semi];
            let ch = match ent {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                num => num
                    .strip_prefix('#')
                    .and_then(|n| n.parse::<u32>().ok())
                    .and_then(char::from_u32),
            };
            ch.map(|c| (c, semi + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Escape the two characters that would otherwise be read as markup in Markdown prose. Text inside
/// code spans is exempt (backticks already make it literal) and goes through [`code_span`] instead.
fn escape_md(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;")
}

/// A Markdown code span, widened when the content itself contains a backtick.
fn code_span(s: &str) -> String {
    if s.contains('`') {
        format!("`` {s} ``")
    } else {
        format!("`{s}`")
    }
}

/// Text content of the first `<tag>…</tag>` in `xml` (no attributes on the tag).
fn inner(xml: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let Some(start) = xml.find(&open) else {
        return String::new();
    };
    let rest = &xml[start + open.len()..];
    let Some(end) = rest.find(&close) else {
        return String::new();
    };
    rest[..end].trim().to_string()
}

/// Text content of a `<tag …>text</tag>` block whose tag carries attributes.
fn block_text(block: &str) -> String {
    let Some(start) = block.find('>') else {
        return String::new();
    };
    let rest = &block[start + 1..];
    let end = rest.rfind("</").unwrap_or(rest.len());
    rest[..end].trim().to_string()
}

/// The `<param … />` entries of a method / signal block, in declaration order.
fn parse_params(block: &str) -> Vec<Param> {
    let mut out = Vec::new();
    let mut rest = block;
    while let Some(pos) = rest.find("<param ") {
        let after = &rest[pos..];
        let end = after.find('>').unwrap_or(after.len());
        let tag = &after[..end];
        out.push(Param {
            name: attr(tag, "name").unwrap_or_default(),
            ty: attr(tag, "type").unwrap_or_default(),
            default: attr(tag, "default").as_deref().map(unescape),
        });
        rest = &after[end..];
    }
    out
}

/// `xreal_camera` -> `XrealCamera`.
fn pascal(stem: &str) -> String {
    stem.split('_')
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut c = s.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// `XrealCameraFeed` -> `xreal_camera_feed`, keeping acronym runs together (`XrealAR` -> `xreal_ar`).
fn snake(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() && i > 0 {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if !prev.is_uppercase() || next_lower {
                out.push('_');
            }
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// The doctool names a script without a `class_name` by its quoted path (`"addons/…/x.gd"`), and uses
/// the same quoted form to qualify that script's enums. Turn either into our display name.
fn name_from_quoted_path(raw: &str) -> Option<String> {
    let path = unescape(raw);
    let path = path.trim_matches('"');
    path.ends_with(".gd").then(|| {
        pascal(
            Path::new(path)
                .file_stem()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default()
                .as_ref(),
        )
    })
}

/// Prettify an `enum="…"` qualifier: `"addons/…/xreal_stream.gd".BlendMode` -> `XrealStream.BlendMode`.
fn pretty_enum(raw: &str) -> String {
    let raw = unescape(raw);
    match raw.rsplit_once('.') {
        Some((qualifier, member)) if qualifier.ends_with(".gd\"") => {
            match name_from_quoted_path(qualifier) {
                Some(cls) => format!("{cls}.{member}"),
                None => raw,
            }
        }
        _ => raw,
    }
}

// ---- Godot's `##`-joining, undone -----------------------------------------------------------

/// `--gdscript-docs` joins every `##` line with a single space, so a doc block's paragraphs and lists
/// arrive as one run-on line: a blank `##` line shows up as a *double* space, and an indented `- ` /
/// `1. ` item just runs into the previous sentence. Rebuild both. The list rule needs two or more
/// markers in the same paragraph before it fires, which keeps ordinary prose (and this addon's em
/// dashes, which are not ASCII ` - `) untouched.
fn unjoin(text: &str) -> String {
    let mut out = String::new();
    for (i, para) in split_paragraphs(text).iter().enumerate() {
        if i > 0 {
            out.push_str("\n\n");
        }
        out.push_str(&relist(para));
    }
    out
}

/// Split on runs of *exactly* two spaces — the doctool's fingerprint for a blank `##` line (one space
/// contributed by the join on either side). A longer run is source alignment that survived the join
/// (`## 2. CONTROL   : TCP-connect …`) and collapses to a single space instead.
fn split_paragraphs(text: &str) -> Vec<String> {
    let mut paras = Vec::new();
    let mut cur = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ' ' && chars.peek() == Some(&' ') {
            let mut run = 1;
            while chars.peek() == Some(&' ') {
                chars.next();
                run += 1;
            }
            if run == 2 {
                if !cur.trim().is_empty() {
                    paras.push(cur.trim().to_string());
                }
                cur.clear();
            } else {
                cur.push(' ');
            }
            continue;
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        paras.push(cur.trim().to_string());
    }
    paras
}

/// Turn ` - ` / ` 1. ` markers back into Markdown list items (see [`unjoin`] for when this fires).
fn relist(para: &str) -> String {
    let marks = list_marks(para);
    if marks.len() < 2 {
        return para.to_string();
    }
    let mut out = String::new();
    let lead = para[..marks[0].0].trim();
    if !lead.is_empty() {
        out.push_str(lead);
        out.push_str("\n\n");
    }
    for (i, &(start, len)) in marks.iter().enumerate() {
        let end = marks.get(i + 1).map_or(para.len(), |&(s, _)| s);
        let marker = para[start..start + len].trim();
        let item = para[start + len..end].trim();
        let _ = writeln!(out, "{marker} {item}");
    }
    out.trim_end().to_string()
}

/// Byte offsets + lengths of the ` - ` / ` <n>. ` list markers in one run-on paragraph.
fn list_marks(para: &str) -> Vec<(usize, usize)> {
    let b = para.as_bytes();
    let mut marks = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b' ' {
            i += 1;
            continue;
        }
        let rest = &b[i + 1..];
        if rest.starts_with(b"- ") {
            marks.push((i, 3));
            i += 3;
            continue;
        }
        let digits = rest.iter().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && rest[digits..].starts_with(b". ") {
            marks.push((i, 1 + digits + 2));
            i += 1 + digits + 2;
            continue;
        }
        i += 1;
    }
    marks
}

// ---- BBCode -> Markdown ---------------------------------------------------------------------

struct Ctx<'a> {
    /// The class whose description is being rendered, for unqualified `[method x]` references.
    class: &'a str,
    ix: &'a Index,
}

/// Render one description from the doc XML as Markdown: Godot BBCode becomes Markdown, member and
/// class references become links, and everything else is escaped prose.
fn markdown(text: &str, ctx: &Ctx) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(pos) = rest.find('[') {
        out.push_str(&escape_md(&unescape(&rest[..pos])));
        rest = &rest[pos..];

        // A rustdoc intra-doc link that gdext passed through: `[`Self::poll_frame`]` reaches the XML
        // as `[[code]Self::poll_frame[/code]]`. Recover the member reference.
        if let Some(after) = rest.strip_prefix("[[code]") {
            if let Some(end) = after.find("[/code]]") {
                let item = &after[..end];
                let target = item.strip_prefix("Self::").unwrap_or(item);
                out.push_str(&member_link("method", target, None, ctx));
                rest = &after[end + "[/code]]".len()..];
                continue;
            }
        }
        // `[code]…[/code]` / `[codeblock]…[/codeblock]` are taken whole so their content stays raw.
        if let Some(md) = take_code(&mut rest) {
            out.push_str(&md);
            continue;
        }

        let Some(close) = rest.find(']') else {
            out.push_str(&escape_md(&unescape(rest)));
            rest = "";
            break;
        };
        let tag = &rest[1..close];
        rest = &rest[close + 1..];
        out.push_str(&render_tag(tag, ctx));
    }
    out.push_str(&escape_md(&unescape(rest)));
    normalize_breaks(&out)
}

/// Consume a `[code]`/`[codeblock]` span at the front of `rest`, returning its Markdown.
fn take_code(rest: &mut &str) -> Option<String> {
    for (open, close, block) in [
        ("[codeblock]", "[/codeblock]", true),
        ("[code]", "[/code]", false),
    ] {
        if let Some(after) = rest.strip_prefix(open) {
            let end = after.find(close)?;
            let body = unescape(&after[..end]);
            *rest = &after[end + close.len()..];
            return Some(if block {
                format!("\n\n```gdscript\n{}\n```\n\n", body.trim())
            } else {
                code_span(&body)
            });
        }
    }
    None
}

/// One BBCode tag. Unknown tags are emitted verbatim — descriptions in this project use bracketed
/// prose (`[u16 length][payload]`, `[yt, ct]`) that must survive untouched.
fn render_tag(tag: &str, ctx: &Ctx) -> String {
    match tag {
        "b" | "/b" => return "**".into(),
        "i" | "/i" => return "*".into(),
        "br" => return "\n".into(),
        "u" | "/u" | "s" | "/s" | "center" | "/center" | "codeblocks" | "/codeblocks"
        | "gdscript" | "/gdscript" | "csharp" | "/csharp" => return String::new(),
        _ => {}
    }
    if let Some(url) = tag.strip_prefix("url=") {
        // `[url=target]` opens; the label follows as prose and `[/url]` closes it.
        return format!("[{url}](");
    }
    if tag == "/url" {
        return ")".into();
    }
    if let Some((kind, target)) = tag.split_once(' ') {
        match kind {
            "param" => return code_span(&unescape(target)),
            "method" | "signal" | "member" | "constant" => {
                return member_link(kind, target, Some(kind), ctx);
            }
            "enum" | "annotation" | "theme_item" => return code_span(&unescape(target)),
            _ => {}
        }
    }
    if is_class_name(tag) {
        return class_link(tag, ctx.ix);
    }
    format!("[{}]", escape_md(&unescape(tag)))
}

/// `[method foo]` / `[member Class.bar]` -> a link, or a code span when the target is not one of ours
/// (a dead link is worse than plain text).
fn member_link(kind: &str, target: &str, labelled: Option<&str>, ctx: &Ctx) -> String {
    let (class, name) = match target.split_once('.') {
        Some((c, n)) => (c.to_string(), n.to_string()),
        None => (ctx.class.to_string(), target.to_string()),
    };
    let qualified = target.contains('.');
    let suffix = if kind == "method" { "()" } else { "" };
    let label = if qualified {
        format!("{class}.{name}{suffix}")
    } else {
        format!("{name}{suffix}")
    };
    match ctx.ix.anchor(kind, &class, &name) {
        Some((slug, anchor)) => {
            let file = if class == ctx.class {
                String::new()
            } else {
                format!("{slug}.md")
            };
            format!("[`{label}`]({file}#{anchor})")
        }
        // Not ours: link the class page when we know it, else leave the reference as plain code.
        None if qualified && !ctx.ix.classes.contains_key(&class) && is_class_name(&class) => {
            format!("[`{label}`]({})", godot_class_url(&class))
        }
        None => {
            let _ = labelled;
            code_span(&label)
        }
    }
}

/// A bare `[Name]` reference is only treated as a class when it looks like one AND is a class this
/// API actually mentions — otherwise bracketed prose would sprout links to pages that do not exist.
fn is_class_name(tag: &str) -> bool {
    let mut chars = tag.chars();
    chars.next().is_some_and(char::is_uppercase) && tag.chars().all(char::is_alphanumeric)
}

fn class_link(name: &str, ix: &Index) -> String {
    match ix.classes.get(name) {
        Some(e) => format!("[`{name}`]({}.md)", e.slug),
        None if ix.engine.contains(name) => format!("[`{name}`]({})", godot_class_url(name)),
        None => code_span(name),
    }
}

/// A type as it appears in a table cell: our classes and the engine's link out, the rest is code.
fn type_link(ty: &str, ix: &Index) -> String {
    match ty.split_once('.') {
        // An enum type (`XrealShared.AudioState`) links to the class page; the enum anchor lives there.
        Some((class, member)) => match ix.classes.get(class) {
            Some(e) => format!("[`{class}.{member}`]({}.md#enum-{member})", e.slug),
            None => code_span(ty),
        },
        None if ty == "void" => code_span(ty),
        None => class_link(ty, ix),
    }
}

/// `[br]` runs become paragraph breaks (two or more) or hard line breaks (one) — except before a list
/// item, which is already its own block and would only collect trailing whitespace.
fn normalize_breaks(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('\n') {
        out.push_str(&rest[..pos]);
        let run = rest[pos..].chars().take_while(|&c| c == '\n').count();
        rest = &rest[pos + run..];
        out.push_str(match (run, starts_list_item(rest)) {
            (1, true) => "\n",
            (1, false) => "  \n",
            _ => "\n\n",
        });
    }
    out.push_str(rest);
    out
}

fn starts_list_item(s: &str) -> bool {
    s.starts_with("- ")
        || s.split_once(". ")
            .is_some_and(|(d, _)| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()))
}

/// Flatten to a single line, for a table cell.
fn one_line(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

/// The lead sentence, for the index table — a class's brief description often runs several sentences
/// and the full text is one click away on its own page.
fn first_sentence(s: &str) -> String {
    let mut out = String::new();
    for part in s.split_inclusive(". ") {
        out.push_str(part);
        let trimmed = out.trim_end();
        // Do not stop on an abbreviation's full stop.
        let tail = trimmed
            .rsplit(' ')
            .next()
            .unwrap_or("")
            .trim_end_matches('.');
        if !matches!(tail, "e.g" | "i.e" | "vs" | "etc" | "cf" | "approx") && trimmed.ends_with('.')
        {
            break;
        }
    }
    let out = out.trim_end();
    if out.len() < s.trim_end().len() {
        format!("{out} …")
    } else {
        out.to_string()
    }
}

// ---- Parsing one class ----------------------------------------------------------------------

struct Origin {
    group: Group,
    script: Option<String>,
    scene: Option<String>,
    unnamed: bool,
    from_gdscript: bool,
}

fn parse_class(xml: &str, name: String, origin: Origin) -> ClassDoc {
    let head = &xml[xml.find("<class ").unwrap_or(0)..];
    let head_end = head.find('>').unwrap_or(head.len());
    let inherits = attr(&head[..head_end], "inherits").unwrap_or_default();

    let methods = blocks(xml, "method")
        .into_iter()
        .filter_map(|b| {
            let tag = &b[..b.find('>').unwrap_or(b.len())];
            let name = attr(tag, "name")?;
            if name.starts_with('_') {
                return None; // private helpers and engine callbacks (_ready, _process, …)
            }
            let ret = b
                .find("<return")
                .and_then(|p| attr(&b[p..], "type"))
                .unwrap_or_else(|| "void".into());
            Some(Method {
                name,
                ret,
                params: parse_params(b),
                is_static: attr(tag, "qualifiers").as_deref() == Some("static"),
                desc: inner(b, "description"),
            })
        })
        .collect::<Vec<_>>();

    let signals = blocks(xml, "signal")
        .into_iter()
        .filter_map(|b| {
            let tag = &b[..b.find('>').unwrap_or(b.len())];
            let name = attr(tag, "name")?;
            (!name.starts_with('_')).then(|| Signal {
                name,
                params: parse_params(b),
                desc: inner(b, "description"),
            })
        })
        .collect::<Vec<_>>();

    let properties = blocks(xml, "member")
        .into_iter()
        .filter_map(|b| {
            let tag = &b[..b.find('>').unwrap_or(b.len())];
            let name = attr(tag, "name")?;
            if name.starts_with('_') {
                return None;
            }
            let ty = match attr(tag, "enum") {
                Some(e) => pretty_enum(&e),
                None => attr(tag, "type").unwrap_or_default(),
            };
            Some(Property {
                name,
                ty,
                // gdext emits `default=""` for a Rust `#[export]` (the value lives in the code, not
                // the docs) — an empty default is "unknown", not "the empty string".
                default: attr(tag, "default")
                    .as_deref()
                    .map(unescape)
                    .filter(|d| !d.is_empty()),
                desc: block_text(b),
            })
        })
        .collect::<Vec<_>>();

    let constants = blocks(xml, "constant")
        .into_iter()
        .filter_map(|b| {
            let tag = &b[..b.find('>').unwrap_or(b.len())];
            let name = attr(tag, "name")?;
            if name.starts_with('_') {
                return None;
            }
            Some(Constant {
                name,
                value: attr(tag, "value")
                    .as_deref()
                    .map(unescape)
                    .unwrap_or_default(),
                enum_name: attr(tag, "enum").as_deref().map(pretty_enum),
                desc: block_text(b),
            })
        })
        .collect::<Vec<_>>();

    let mut doc = ClassDoc {
        slug: snake(&name),
        name,
        inherits,
        group: origin.group,
        script: origin.script,
        scene: origin.scene,
        unnamed: origin.unnamed,
        brief: inner(xml, "brief_description"),
        description: inner(xml, "description"),
        properties,
        methods,
        signals,
        constants,
        from_gdscript: origin.from_gdscript,
    };
    doc.methods.sort_by(|a, b| a.name.cmp(&b.name));
    doc.signals.sort_by(|a, b| a.name.cmp(&b.name));
    doc.properties.sort_by(|a, b| a.name.cmp(&b.name));
    doc
}

// ---- Rendering ------------------------------------------------------------------------------

const HEADER: &str = "<!-- Generated by scripts/gen_api_docs.ps1 (Windows) / gen_api_docs.sh (mac/Linux) from the\n     `///` doc comments in src/ and the `##` doc comments in addons/godot_xreal/ — DO NOT EDIT. -->\n";

fn signature(m: &Method) -> String {
    let params = params_text(&m.params);
    let prefix = if m.is_static { "static " } else { "" };
    if m.ret == "void" || m.ret.is_empty() {
        format!("{prefix}{}({params})", m.name)
    } else {
        format!("{prefix}{}({params}) -> {}", m.name, m.ret)
    }
}

fn params_text(params: &[Param]) -> String {
    params
        .iter()
        .map(|p| match &p.default {
            Some(d) => format!("{}: {} = {}", p.name, p.ty, d),
            None => format!("{}: {}", p.name, p.ty),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// One class page. Members are anchored explicitly (`<a id="method-x">`) so cross-references stay
/// stable no matter how a static-site generator slugifies the headings.
fn render_class(c: &ClassDoc, ix: &Index) -> String {
    let ctx = Ctx { class: &c.name, ix };
    let prose = |text: &str| {
        let text = if c.from_gdscript {
            unjoin(text)
        } else {
            text.to_string()
        };
        markdown(&text, &ctx)
    };

    let mut s = String::new();
    s.push_str(HEADER);
    let _ = writeln!(s, "\n# {}\n", c.name);

    if !c.inherits.is_empty() {
        let _ = writeln!(s, "**Inherits:** {}  ", type_link(&c.inherits, ix));
    }
    if let Some(scene) = &c.scene {
        let _ = writeln!(s, "**Scene:** `{scene}`  ");
    }
    if let Some(script) = &c.script {
        let _ = writeln!(s, "**Script:** `{script}`  ");
    } else {
        s.push_str("**Registered by:** the `godot_xreal` GDExtension  \n");
    }
    if c.unnamed {
        let how = match (&c.scene, &c.script) {
            (Some(scene), _) => format!("Use it by instancing `{scene}`"),
            (None, Some(script)) => format!("Attach `{script}` to a node of your own"),
            (None, None) => "Load the script directly".to_string(),
        };
        let _ = writeln!(
            s,
            "\n> The script declares no `class_name`, so **{}** is this reference's name for it, not \
             a type you can write in GDScript. {how}.",
            c.name
        );
    }

    if !c.brief.is_empty() {
        let _ = writeln!(s, "\n{}", prose(&c.brief));
    }
    if !c.description.is_empty() {
        let _ = writeln!(s, "\n{}", prose(&c.description));
    }

    if !c.properties.is_empty() {
        s.push_str("\n## Properties\n");
        for p in &c.properties {
            let _ = write!(s, "\n<a id=\"property-{}\"></a>\n\n### ", p.name);
            match &p.default {
                Some(d) => {
                    let _ = writeln!(s, "{}: {} = {}\n", p.name, p.ty, d);
                }
                None => {
                    let _ = writeln!(s, "{}: {}\n", p.name, p.ty);
                }
            }
            if !p.desc.is_empty() {
                let _ = writeln!(s, "{}", prose(&p.desc));
            }
        }
    }

    if !c.signals.is_empty() {
        s.push_str("\n## Signals\n");
        for sig in &c.signals {
            let _ = write!(s, "\n<a id=\"signal-{}\"></a>\n\n", sig.name);
            let _ = writeln!(s, "### {}({})\n", sig.name, params_text(&sig.params));
            if !sig.desc.is_empty() {
                let _ = writeln!(s, "{}", prose(&sig.desc));
            }
        }
    }

    if !c.methods.is_empty() {
        s.push_str("\n## Methods\n");
        for m in &c.methods {
            let _ = write!(s, "\n<a id=\"method-{}\"></a>\n\n", m.name);
            let _ = writeln!(s, "### {}\n", signature(m));
            if !m.desc.is_empty() {
                let _ = writeln!(s, "{}", prose(&m.desc));
            }
        }
    }

    render_constants(&mut s, c, &prose);
    s
}

/// Enum members grouped under their enum (like the engine's own reference), then the loose constants.
fn render_constants(s: &mut String, c: &ClassDoc, prose: &dyn Fn(&str) -> String) {
    let mut enums: Vec<(&str, Vec<&Constant>)> = Vec::new();
    for k in &c.constants {
        let Some(name) = k.enum_name.as_deref() else {
            continue;
        };
        let short = name.rsplit_once('.').map_or(name, |(_, m)| m);
        match enums.iter_mut().find(|(n, _)| *n == short) {
            Some((_, items)) => items.push(k),
            None => enums.push((short, vec![k])),
        }
    }
    if !enums.is_empty() {
        s.push_str("\n## Enumerations\n");
        for (name, items) in &enums {
            let _ = write!(s, "\n<a id=\"enum-{name}\"></a>\n\n### enum {name}\n");
            render_const_group(s, items, 4, prose);
        }
    }

    let loose: Vec<&Constant> = c
        .constants
        .iter()
        .filter(|k| k.enum_name.is_none())
        .collect();
    if !loose.is_empty() {
        s.push_str("\n## Constants\n");
        render_const_group(s, &loose, 3, prose);
    }
}

/// A table when every description fits in a cell, else one anchored section each — a few constants
/// carry paragraphs of rationale, which a table cell would run off the page.
fn render_const_group(
    s: &mut String,
    items: &[&Constant],
    level: usize,
    prose: &dyn Fn(&str) -> String,
) {
    let rendered: Vec<String> = items.iter().map(|k| prose(&k.desc)).collect();
    let tabular = rendered.iter().all(|d| d.len() <= 220 && !d.contains('\n'));

    if tabular {
        s.push_str("\n| Constant | Value | Description |\n| --- | --- | --- |\n");
        for (k, desc) in items.iter().zip(&rendered) {
            let _ = writeln!(
                s,
                "| <a id=\"constant-{}\"></a>`{}` | `{}` | {} |",
                k.name,
                k.name,
                k.value,
                one_line(desc)
            );
        }
        return;
    }
    let hashes = "#".repeat(level);
    for (k, desc) in items.iter().zip(&rendered) {
        let _ = write!(s, "\n<a id=\"constant-{}\"></a>\n\n", k.name);
        let _ = writeln!(s, "{hashes} {} = {}\n", k.name, k.value);
        if !desc.is_empty() {
            let _ = writeln!(s, "{desc}");
        }
    }
}

/// The landing page: every class grouped by where it comes from.
fn render_index(classes: &[ClassDoc], ix: &Index) -> String {
    let mut s = String::new();
    s.push_str(HEADER);
    s.push_str(
        "\n# API reference\n\n\
         Every class `godot_xreal` exposes to GDScript, generated from the doc comments in the sources:\n\
         the native classes from the Rust `///` comments (via gdext's `register-docs`), the feature\n\
         components from the GDScript `##` comments (via `godot --doctool --gdscript-docs`). The same\n\
         text is what the editor shows on F1.\n\n\
         Regenerate with `pwsh scripts/gen_api_docs.ps1` (Windows) or `bash scripts/gen_api_docs.sh`\n\
         (mac/Linux) after changing a doc comment.\n",
    );

    for (group, title, blurb) in [
        (
            Group::Native,
            "Native classes",
            "Registered by the GDExtension — available as global types on device.",
        ),
        (
            Group::Feature,
            "Feature components",
            "Drop-in scenes under `addons/godot_xreal/features/`: instance one, flip its `enabled` \
             property (or call `set_enabled()`), and connect its signals.",
        ),
        (
            Group::Helper,
            "Helpers",
            "Supporting GDScript classes in the addon.",
        ),
    ] {
        let members: Vec<&ClassDoc> = classes.iter().filter(|c| c.group == group).collect();
        if members.is_empty() {
            continue;
        }
        let _ = write!(s, "\n## {title}\n\n{blurb}\n\n");
        s.push_str("| Class | Inherits | Description |\n| --- | --- | --- |\n");
        for c in members {
            let ctx = Ctx {
                class: &c.name,
                ix,
            };
            let brief = if c.from_gdscript {
                unjoin(&c.brief)
            } else {
                c.brief.clone()
            };
            let _ = writeln!(
                s,
                "| [{}]({}.md) | {} | {} |",
                c.name,
                c.slug,
                type_link(&c.inherits, ix),
                first_sentence(&one_line(&markdown(&brief, &ctx)))
            );
        }
    }
    s
}

// ---- Assembly -------------------------------------------------------------------------------

/// Every `.gd` file under `addons/godot_xreal/`, repo-relative with forward slashes.
fn addon_scripts(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.join("addons/godot_xreal")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "gd") {
                if let Ok(rel) = path.strip_prefix(root) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    out.sort();
    out
}

/// Which script declares `class_name X`, for the doctool's named-class XML (which records the name
/// but not the path).
fn class_name_paths(root: &Path, scripts: &[String]) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for rel in scripts {
        let Ok(src) = std::fs::read_to_string(root.join(rel)) else {
            continue;
        };
        if let Some(name) = src
            .lines()
            .find_map(|l| l.strip_prefix("class_name ").map(str::trim))
        {
            map.insert(name.to_string(), rel.clone());
        }
    }
    map
}

fn parse_gdscript_classes(root: &Path, xml_dir: &Path) -> Vec<ClassDoc> {
    let scripts = addon_scripts(root);
    let named = class_name_paths(root, &scripts);

    let mut files: Vec<PathBuf> = std::fs::read_dir(xml_dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", xml_dir.display()))
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "xml"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "no GDScript doc XML in {} — run Godot's `--doctool <dir> --gdscript-docs res://addons/godot_xreal` first",
        xml_dir.display()
    );

    let mut out = Vec::new();
    for path in files {
        let xml = std::fs::read_to_string(&path).expect("read GDScript doc XML");
        let head = &xml[xml.find("<class ").expect("no <class> in doc XML")..];
        let raw_name =
            attr(&head[..head.find('>').unwrap_or(head.len())], "name").unwrap_or_default();

        let (name, script, unnamed) = match name_from_quoted_path(&raw_name) {
            Some(name) => {
                let path = unescape(&raw_name).trim_matches('"').to_string();
                (name, path, true)
            }
            None => {
                let script = named.get(&raw_name).cloned().unwrap_or_default();
                (raw_name, script, false)
            }
        };
        if script.is_empty() || EXCLUDED_SCRIPTS.contains(&script.as_str()) {
            continue;
        }
        let scene_rel = script.replace(".gd", ".tscn");
        let scene = root.join(&scene_rel).exists().then_some(scene_rel);
        let group = if script.contains("/features/") {
            Group::Feature
        } else {
            Group::Helper
        };
        out.push(parse_class(
            &xml,
            name,
            Origin {
                group,
                script: Some(script),
                scene,
                unnamed,
                from_gdscript: true,
            },
        ));
    }
    out
}

fn build_index(classes: &[ClassDoc]) -> Index {
    let mut ix = Index {
        classes: BTreeMap::new(),
        engine: BTreeSet::new(),
    };
    for c in classes {
        ix.classes.insert(
            c.name.clone(),
            Entry {
                slug: c.slug.clone(),
                methods: c.methods.iter().map(|m| m.name.clone()).collect(),
                signals: c.signals.iter().map(|s| s.name.clone()).collect(),
                properties: c.properties.iter().map(|p| p.name.clone()).collect(),
                constants: c.constants.iter().map(|k| k.name.clone()).collect(),
            },
        );
    }
    // Only the engine types this API actually names are link targets — see `is_class_name`.
    let mut note = |ty: &str| {
        if is_class_name(ty) && !ix.classes.contains_key(ty) {
            ix.engine.insert(ty.to_string());
        }
    };
    for c in classes {
        note(&c.inherits);
        for m in &c.methods {
            note(&m.ret);
            m.params.iter().for_each(|p| note(&p.ty));
        }
        for s in &c.signals {
            s.params.iter().for_each(|p| note(&p.ty));
        }
        for p in &c.properties {
            note(&p.ty);
        }
    }
    ix
}

/// All of `docs/api/`: one page per class plus the index, keyed by file name.
fn generate(root: &Path, xml_dir: &Path) -> BTreeMap<String, String> {
    let mut classes: Vec<ClassDoc> = cleaned_classes()
        .into_iter()
        .map(|(name, xml)| {
            parse_class(
                &xml,
                name,
                Origin {
                    group: Group::Native,
                    script: None,
                    scene: None,
                    unnamed: false,
                    from_gdscript: false,
                },
            )
        })
        .collect();
    classes.extend(parse_gdscript_classes(root, xml_dir));
    classes.sort_by(|a, b| (a.group, &a.name).cmp(&(b.group, &b.name)));

    let ix = build_index(&classes);
    let mut pages = BTreeMap::new();
    for c in &classes {
        let page = render_class(c, &ix);
        assert!(
            pages.insert(format!("{}.md", c.slug), page).is_none(),
            "two classes map to docs/api/{}.md",
            c.slug
        );
    }
    pages.insert("README.md".to_string(), render_index(&classes, &ix));
    pages
}

#[test]
fn api_docs() {
    let Some(mode) = std::env::var_os("XREAL_API_DOCS") else {
        return; // inert during normal `cargo test`
    };
    let mode = mode.to_string_lossy().to_string();
    let xml_dir = PathBuf::from(std::env::var_os("XREAL_GDSCRIPT_XML").expect(
        "XREAL_API_DOCS needs XREAL_GDSCRIPT_XML=<dir of Godot's --gdscript-docs XML>; \
         scripts/gen_api_docs.{ps1,sh} produce it",
    ));
    let root = manifest_dir();
    let out_dir = root.join("docs/api");
    let pages = generate(&root, &xml_dir);

    let existing: BTreeSet<String> = std::fs::read_dir(&out_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".md"))
        .collect();
    let stale: Vec<&String> = existing
        .iter()
        .filter(|n| !pages.contains_key(*n))
        .collect();

    match mode.as_str() {
        "write" => {
            std::fs::create_dir_all(&out_dir).expect("create docs/api");
            for (name, body) in &pages {
                std::fs::write(out_dir.join(name), body).expect("write class reference page");
            }
            for name in &stale {
                std::fs::remove_file(out_dir.join(name)).expect("remove stale page");
            }
            eprintln!(
                "[api_docs] wrote {} pages to docs/api ({} stale removed)",
                pages.len(),
                stale.len()
            );
        }
        "check" => {
            let mut drift: Vec<String> = pages
                .iter()
                .filter(|(name, body)| {
                    std::fs::read_to_string(out_dir.join(name)).unwrap_or_default() != **body
                })
                .map(|(name, _)| format!("docs/api/{name}"))
                .collect();
            drift.extend(stale.iter().map(|n| format!("docs/api/{n} (stale)")));
            assert!(
                drift.is_empty(),
                "the class reference is out of sync with the doc comments — run \
                 scripts/gen_api_docs and commit:\n  {}",
                drift.join("\n  ")
            );
            eprintln!("[api_docs] class reference in sync ({} pages)", pages.len());
        }
        other => panic!("XREAL_API_DOCS must be 'write' or 'check', got {other:?}"),
    }
}
