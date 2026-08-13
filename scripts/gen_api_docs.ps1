#!/usr/bin/env pwsh
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
# Windows twin of gen_api_docs.sh (mac/Linux; no pwsh there). Do not cross-call.
#
#   pwsh scripts/gen_api_docs.ps1                      # (re)write docs/user/api/
#   pwsh scripts/gen_api_docs.ps1 -Check               # verify the committed pages are in sync
#
# The Godot binary is a build variable, resolved in SCons order: an explicit command-line value
# wins over the environment, which wins over the default:
#
#   pwsh scripts/gen_api_docs.ps1 -Godot 'C:\path\Godot_v4.7-stable_win64.exe'
#   pwsh scripts/gen_api_docs.ps1 godot='C:\path\Godot_v4.7-stable_win64.exe'   # SCons-style
#   $env:GODOT = 'C:\path\Godot_v4.7-stable_win64.exe'; pwsh scripts/gen_api_docs.ps1
#   (default: `godot`, i.e. on PATH, the same variable name the build scripts use)
param(
	[switch]$Check,
	[string]$Godot,
	[Parameter(ValueFromRemainingArguments = $true)][string[]]$Vars
)
$ErrorActionPreference = 'Stop'

# A SCons-style `godot=<path>` is accepted as well as `-Godot <path>`: PowerShell binds the first
# positional argument to -Godot, so unwrap it there too, not only in the remaining arguments.
if ($Godot -match '^(?i)godot=(.+)$') { $Godot = $Matches[1] }
foreach ($v in $Vars) {
	if ($v -match '^(?i)godot=(.+)$') { $Godot = $Matches[1] }
	else { throw "unknown argument '$v' (expected godot=<path>)" }
}
if (-not $Godot) { $Godot = if ($env:GODOT) { $env:GODOT } else { 'godot' } }

$root = Split-Path -Parent $PSScriptRoot
$mode = if ($Check) { 'check' } else { 'write' }

# The doctool merges into existing files, so hand it a fresh directory every run.
$xmlDir = Join-Path ([System.IO.Path]::GetTempPath()) "xreal-gdscript-docs-$PID"
New-Item -ItemType Directory -Force -Path $xmlDir | Out-Null

Push-Location $root
try {
	# A missing binary throws here; fold it into the same hint rather than a bare CommandNotFound.
	$ver = try { & $Godot --version 2>&1 | Select-Object -Last 1 } catch { '' }
	if ("$ver" -notmatch '^4\.7') {
		throw "Godot must be 4.7 ('$Godot --version' = '$ver'). Pass -Godot <path> / godot=<path>, or set `$env:GODOT."
	}

	# Import first. --doctool walks the project's imported script list, which does not exist until
	# the project has been opened once: in a fresh clone or a NEW GIT WORKTREE there is no .godot/,
	# so doctool finds nothing under res://addons/godot_xreal, writes no GDScript XML, and the
	# renderer then deletes all fifteen of those pages as stale (seen 2026-08-13 in a new worktree).
	# The import is incremental, so on an already-imported tree it costs about a second.
	$out = & $Godot --headless --path $root --import 2>&1 | Out-String
	if ($LASTEXITCODE -ne 0) { Write-Host $out; throw "godot --import failed" }

	# --gdscript-docs documents every script it finds, and src/api_docs.rs drops the editor-only ones.
	# Pipe the output through a cmdlet, Out-String, rather than simply assigning it: the Windows Godot
	# build is a GUI-subsystem binary, so a bare call, even `$out = & godot …`, hands control back
	# before the process has written anything and the next step reads an empty directory. Only a real
	# pipeline blocks on the child's stdout until it exits. It is kept in a variable so a success
	# stays quiet.
	$out = & $Godot --headless --path $root --doctool $xmlDir --gdscript-docs res://addons/godot_xreal 2>&1 | Out-String
	if ($LASTEXITCODE -ne 0) { Write-Host $out; throw "godot --doctool --gdscript-docs failed" }

	$env:XREAL_API_DOCS = $mode
	$env:XREAL_GDSCRIPT_XML = $xmlDir
	cargo test --lib api_docs -- --nocapture
	if ($LASTEXITCODE -ne 0) { throw "api_docs failed (mode=$mode)" }
} finally {
	Remove-Item Env:\XREAL_API_DOCS -ErrorAction SilentlyContinue
	Remove-Item Env:\XREAL_GDSCRIPT_XML -ErrorAction SilentlyContinue
	Remove-Item -Recurse -Force $xmlDir -ErrorAction SilentlyContinue
	Pop-Location
}
