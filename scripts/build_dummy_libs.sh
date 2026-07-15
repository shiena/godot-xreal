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

build() { # triple out extra-flags…
	local triple="$1" out="$2"
	shift 2
	# -Wl,-noentry (dash form) — the slash form is mangled by MSYS path conversion.
	"$CLANG" --target="$triple" -O2 -ffreestanding -nostdlib -shared -fuse-ld=lld "$@" \
		-o "$root/dummy/$out" "$src"
	echo "built $out"
}

build x86_64-pc-windows-msvc    godot_xreal_dummy.windows.x86_64.dll   -Wl,-noentry
build aarch64-pc-windows-msvc   godot_xreal_dummy.windows.arm64.dll    -Wl,-noentry
build x86_64-unknown-linux-gnu  libgodot_xreal_dummy.linux.x86_64.so   -fPIC
build aarch64-unknown-linux-gnu libgodot_xreal_dummy.linux.arm64.so    -fPIC
# lld ad-hoc-codesigns arm64 Mach-O output (mandatory on Apple Silicon).
build arm64-apple-macos11       libgodot_xreal_dummy.macos.arm64.dylib
build x86_64-apple-macos11      libgodot_xreal_dummy.macos.x86_64.dylib

# lld-link emits an import .lib next to each DLL; the stubs are dlopen-only.
rm -f "$root"/dummy/*.lib
