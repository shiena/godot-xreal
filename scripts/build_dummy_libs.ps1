#!/usr/bin/env pwsh
# Cross-compile the desktop dummy GDExtension stubs (dummy/gdext_dummy.c) for every
# desktop platform the Godot editor runs on. The stubs stop the editor's "No GDExtension
# library found for current OS and architecture" error on this Android-only extension;
# they accept initialization and register nothing (see the comment in gdext_dummy.c).
#
# The source is freestanding (no libc, no SDKs), so a single clang + lld cross-compiles
# all six targets from any host. The binaries are tiny and NOT committed — run this once
# after cloning; rerun only if gdext_dummy.c or the entry_symbol changes.
#
#   pwsh scripts/build_dummy_libs.ps1              # clang on PATH
#   pwsh scripts/build_dummy_libs.ps1 -Clang <path-to-clang>
#
# Env override: CLANG.
param(
	[string]$Clang = $(if ($env:CLANG) { $env:CLANG } else { 'clang' })
)
$ErrorActionPreference = 'Stop'

if (-not (Get-Command $Clang -ErrorAction SilentlyContinue)) {
	throw "clang not found ('$Clang') — install LLVM (e.g. 'scoop install llvm') or pass -Clang / set `$env:CLANG."
}

$root = Split-Path -Parent $PSScriptRoot
$src = Join-Path $root 'dummy/gdext_dummy.c'

# -Wl,-noentry: no CRT means no DllMainCRTStartup; a resident DLL needs no entry point.
$targets = @(
	@{ triple = 'x86_64-pc-windows-msvc';    out = 'godot_xreal_dummy.windows.x86_64.dll';   extra = @('-Wl,-noentry') },
	@{ triple = 'aarch64-pc-windows-msvc';   out = 'godot_xreal_dummy.windows.arm64.dll';    extra = @('-Wl,-noentry') },
	@{ triple = 'x86_64-unknown-linux-gnu';  out = 'libgodot_xreal_dummy.linux.x86_64.so';   extra = @('-fPIC') },
	@{ triple = 'aarch64-unknown-linux-gnu'; out = 'libgodot_xreal_dummy.linux.arm64.so';    extra = @('-fPIC') },
	# lld ad-hoc-codesigns arm64 Mach-O output (mandatory on Apple Silicon).
	@{ triple = 'arm64-apple-macos11';       out = 'libgodot_xreal_dummy.macos.arm64.dylib'; extra = @() },
	@{ triple = 'x86_64-apple-macos11';      out = 'libgodot_xreal_dummy.macos.x86_64.dylib'; extra = @() }
)

foreach ($t in $targets) {
	$out = Join-Path $root "dummy/$($t.out)"
	& $Clang "--target=$($t.triple)" -O2 -ffreestanding -nostdlib -shared -fuse-ld=lld @($t.extra) -o $out $src
	if ($LASTEXITCODE -ne 0) { throw "clang failed for $($t.triple)" }
	Write-Host "built $($t.out)"
}

# lld-link emits an import .lib next to each DLL; the stubs are dlopen-only.
Remove-Item (Join-Path $root 'dummy/*.lib') -ErrorAction SilentlyContinue
