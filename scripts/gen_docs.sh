#!/usr/bin/env bash
# Regenerate the GDScript API documentation from the single source of truth — the `///` doc
# comments in the .rs sources under src/ — into two artifacts #included by the desktop dummy:
#   dummy/stub_docs.inc     every class's reference XML (descriptions), embedded as C literals
#   dummy/stub_members.inc  every registered method / signal / constant + signature (data tables)
# The dummy registers the members and loads the XML so the editor F1 help shows the full API.
#
# The generator itself is in-crate Rust (src/doc_gen.rs): gdext's `register-docs` assembles the XML
# from the doc comments and `godot::docs::gather_xml_docs` hands it over without a live engine, so
# this runs it as a host `cargo test`. This is a thin, cross-platform entry point — the Rust side
# produces the bytes, so the .ps1 twin needs no output-parity dance. Needs a host Rust toolchain.
#
# POSIX twin of gen_docs.ps1 (Windows; no pwsh on mac/Linux). Do not cross-call.
#
#   ./scripts/gen_docs.sh            # (re)write the doc artifacts
#   ./scripts/gen_docs.sh --check    # verify the committed artifacts are in sync (CI)
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
mode=write
[ "${1:-}" = "--check" ] && mode=check

cd "$root"
XREAL_DOC_GEN="$mode" cargo test --lib doc_gen -- --nocapture
