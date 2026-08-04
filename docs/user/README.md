# docs/user/

User-facing documentation. Start with the repo [README](../../README.md) for what the addon does
and how to install it; this folder holds the class reference.

## api/: generated class reference

[api/README.md](api/README.md) lists every class the addon exposes to GDScript, one page each. Those
pages are **generated, not written**: the native classes come from the `///` doc comments in `src/`
(gdext `register-docs`) and the feature components from the `##` doc comments in
`addons/godot_xreal/` (Godot's own `--gdscript-docs` doctool), rendered by `src/api_docs.rs`. Edit a
doc comment, then run `scripts/gen_api_docs.{ps1,sh}` and commit; the pages carry a DO-NOT-EDIT
header for a reason. They are plain CommonMark with explicit anchors, so they read on GitHub as-is
and any static-site generator can build them unchanged.
