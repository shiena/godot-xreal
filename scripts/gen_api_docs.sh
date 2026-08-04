#!/usr/bin/env bash
# Regenerate the Markdown class reference in docs/user/api/, one page per class the addon exposes to
# GDScript, from the doc comments as the single source of truth:
#   src/*.rs                     `///` comments -> gdext `register-docs` XML (src/api_docs.rs reads it)
#   addons/godot_xreal/**/*.gd   `##`  comments -> Godot's own doctool, run below
# Both halves are the same Godot class-reference XML, so one renderer (src/api_docs.rs) covers both
# and the pages cross-link into each other. Output is plain CommonMark with explicit <a id> anchors,
# so it renders on GitHub as-is and any static-site generator can consume it unchanged.
#
# Needs a host Rust toolchain AND a Godot 4.7 binary (the GDScript half is the editor's doctool).
# CI has no Godot, hence the policy: generate locally and commit, like dummy/stub_docs.inc.
#
# POSIX twin of gen_api_docs.ps1 (Windows; no pwsh on mac/Linux). Do not cross-call.
#
#   ./scripts/gen_api_docs.sh            # (re)write docs/user/api/
#   ./scripts/gen_api_docs.sh --check    # verify the committed pages are in sync
#
# The Godot binary is a build variable, resolved in SCons order: an explicit command-line value
# wins over the environment, which wins over the default:
#
#   ./scripts/gen_api_docs.sh godot=/path/to/godot     # SCons-style
#   ./scripts/gen_api_docs.sh --godot /path/to/godot
#   GODOT=/path/to/godot ./scripts/gen_api_docs.sh
#   (default: `godot`, i.e. on PATH, the same variable name the build scripts use)
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
godot="${GODOT:-godot}"
mode=write

usage() { echo "usage: $0 [--check] [godot=<path> | --godot <path>]" >&2; exit 2; }
while [ $# -gt 0 ]; do
	case "$1" in
		--check) mode=check ;;
		godot=*) godot="${1#godot=}" ;;
		--godot=*) godot="${1#--godot=}" ;;
		--godot) shift; [ $# -gt 0 ] || usage; godot="$1" ;;
		-h|--help) usage ;;
		*) echo "unknown argument '$1'" >&2; usage ;;
	esac
	shift
done

# The doctool merges into existing files, so hand it a fresh directory every run.
xml_dir="$(mktemp -d "${TMPDIR:-/tmp}/xreal-gdscript-docs.XXXXXX")"
trap 'rm -rf "$xml_dir"' EXIT

cd "$root"
# A missing binary lands in the same hint below rather than a bare "command not found".
ver="$("$godot" --version 2>/dev/null | tail -n1 || true)"
case "$ver" in
	4.7*) ;;
	*) echo "Godot must be 4.7 ('$godot --version' = '$ver'). Pass godot=<path>, or set GODOT." >&2; exit 1 ;;
esac

# --gdscript-docs documents every script it finds; src/api_docs.rs drops the editor-only ones.
"$godot" --headless --path "$root" --doctool "$xml_dir" --gdscript-docs res://addons/godot_xreal

XREAL_API_DOCS="$mode" XREAL_GDSCRIPT_XML="$xml_dir" cargo test --lib api_docs -- --nocapture
