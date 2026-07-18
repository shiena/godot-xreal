#!/usr/bin/env pwsh
# Regenerate the GDScript API documentation from the single source of truth — the `///` doc
# comments in the .rs sources under src/ — into two artifacts #included by the desktop dummy:
#   dummy/stub_docs.inc     every class's reference XML (descriptions), embedded as C literals
#   dummy/stub_members.inc  every registered method / signal / constant + signature (data tables)
# The dummy registers the members and loads the XML so the editor F1 help shows the full API.
#
# The generator itself is in-crate Rust (src/doc_gen.rs): gdext's `register-docs` assembles the XML
# from the doc comments and `godot::docs::gather_xml_docs` hands it over without a live engine, so
# this runs it as a host `cargo test`. This is a thin, cross-platform entry point — the Rust side
# produces the bytes, so this twin needs no output-parity dance. Needs a host Rust toolchain.
#
# Windows twin of gen_docs.sh (mac/Linux; no pwsh there). Do not cross-call.
#
#   pwsh scripts/gen_docs.ps1            # (re)write the doc artifacts
#   pwsh scripts/gen_docs.ps1 -Check     # verify the committed artifacts are in sync (CI)
param([switch]$Check)
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$mode = if ($Check) { 'check' } else { 'write' }

Push-Location $root
try {
	$env:XREAL_DOC_GEN = $mode
	cargo test --lib doc_gen -- --nocapture
	if ($LASTEXITCODE -ne 0) { throw "doc_gen failed (mode=$mode)" }
} finally {
	Remove-Item Env:\XREAL_DOC_GEN -ErrorAction SilentlyContinue
	Pop-Location
}
