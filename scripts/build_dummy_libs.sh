#!/usr/bin/env bash
# Cross-compile the desktop dummy GDExtension stubs (dummy/gdext_dummy.c) for every
# desktop platform the Godot editor runs on. See build_dummy_libs.ps1 for the full story;
# this is its port for Git Bash / macOS / Linux.
#
#   ./scripts/build_dummy_libs.sh          # clang on PATH
#   CLANG=<path-to-clang> ./scripts/build_dummy_libs.sh
set -euo pipefail

CLANG="${CLANG:-clang}"
command -v "$CLANG" >/dev/null 2>&1 || {
	echo "clang not found ('$CLANG') — install LLVM or set CLANG=…" >&2
	exit 1
}

root="$(cd "$(dirname "$0")/.." && pwd)"
src="$root/dummy/gdext_dummy.c"

# Regenerate the placeholder class list from the Rust source (single source of truth).
bash "$root/scripts/gen_stub_classes.sh"
# gdext_dummy.c also #includes dummy/stub_docs.inc (the class-ref docs shown in the editor F1 help).
# That file is a committed prerequisite regenerated separately by scripts/gen_docs.sh (it needs a
# Rust host toolchain, kept out of this clang-only build); CI checks it stays in sync.

# Built into per-platform folders under the addon's bin/ (Godot convention); the .gdextension
# points its desktop entries there. Gitignored — built locally, not committed.
bin_root="$root/addons/godot_xreal/bin"

build() { # triple out extra-flags…
	local triple="$1" out="$2"
	shift 2
	mkdir -p "$(dirname "$bin_root/$out")"
	# -Wl,-noentry (dash form) — the slash form is mangled by MSYS path conversion.
	# -fno-stack-protector: the freestanding build has no __stack_chk_fail / __stack_chk_guard to
	# link against, and some targets (e.g. macOS) enable the stack protector by default for
	# functions with local buffers (register_members' PropertyInfo arrays).
	"$CLANG" --target="$triple" -O2 -ffreestanding -nostdlib -fno-stack-protector -shared -fuse-ld=lld "$@" \
		-o "$bin_root/$out" "$src"
	echo "built $out"
}

build x86_64-pc-windows-msvc    windows/godot_xreal_dummy.x86_64.dll   -Wl,-noentry
build aarch64-pc-windows-msvc   windows/godot_xreal_dummy.arm64.dll    -Wl,-noentry
build x86_64-unknown-linux-gnu  linux/libgodot_xreal_dummy.x86_64.so   -fPIC
build aarch64-unknown-linux-gnu linux/libgodot_xreal_dummy.arm64.so    -fPIC
# lld ad-hoc-codesigns arm64 Mach-O output (mandatory on Apple Silicon).
build arm64-apple-macos11       macos/libgodot_xreal_dummy.arm64.dylib
build x86_64-apple-macos11      macos/libgodot_xreal_dummy.x86_64.dylib

# lld-link emits an import .lib next to each DLL; the stubs are dlopen-only.
rm -f "$bin_root"/windows/*.lib
