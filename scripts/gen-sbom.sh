#!/usr/bin/env bash
# Generate a CycloneDX 1.5 SBOM for the `xfade` CLI.
#
# cargo-cyclonedx resolves the whole workspace, so its raw output also lists
# crates that only other members (the GUI) or unused optional features pull
# in. A scanner would then report advisories for code that is never shipped in
# the CLI. We therefore intersect its component list with the real build graph
# reported by `cargo tree` for every target.
#
# Usage: scripts/gen-sbom.sh [output-file]   (default: xfade.cdx.json)
# Requires: cargo, cargo-cyclonedx, python3
set -euo pipefail

OUT="${1:-xfade.cdx.json}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
CDX_NAME="xfade-raw.cdx"

# Generated next to each workspace member's Cargo.toml; only the CLI's is used.
cargo cyclonedx \
  --manifest-path crates/cli/Cargo.toml \
  --format json --spec-version 1.5 --target all \
  --override-filename "$CDX_NAME" >/dev/null

RAW="crates/cli/${CDX_NAME}.json"
cleanup_raw() {
  rm -f crates/cli/"${CDX_NAME}".json crates/core/"${CDX_NAME}".json \
    crates/gui/src-tauri/"${CDX_NAME}".json
}
trap 'cleanup_raw; rm -rf "$TMP"' EXIT

# `{p}` prints "name vX.Y.Z (source)"; normal+build deps for all targets
# (dev-dependencies are not part of the shipped binary).
cargo tree -p xfade --target all -e normal,build --prefix none --format '{p}' \
  | sort -u > "$TMP/build-graph.txt"

python3 - "$RAW" "$TMP/build-graph.txt" "$OUT" <<'PY'
import json, re, sys

raw_path, graph_path, out_path = sys.argv[1:4]
bom = json.load(open(raw_path))

built = set()
for line in open(graph_path):
    m = re.match(r"^(\S+) v(\S+)", line.strip())
    if m:
        built.add((m.group(1), m.group(2)))

root_ref = bom["metadata"]["component"].get("bom-ref")
kept, dropped = [], []
for c in bom.get("components", []):
    (kept if (c["name"], c["version"]) in built else dropped).append(c)

kept_refs = {c["bom-ref"] for c in kept if "bom-ref" in c}
if root_ref:
    kept_refs.add(root_ref)

bom["components"] = kept
deps = []
for d in bom.get("dependencies", []):
    if d["ref"] not in kept_refs:
        continue
    d = dict(d)
    d["dependsOn"] = [r for r in d.get("dependsOn", []) if r in kept_refs]
    deps.append(d)
bom["dependencies"] = deps

# Sanity: the CLI's direct, security-relevant dependencies must survive.
names = {c["name"] for c in kept}
for must in ("rustls", "rusqlite", "tokio", "axum", "reqwest"):
    if must not in names:
        sys.exit(f"SBOM sanity check failed: {must} missing after pruning")

json.dump(bom, open(out_path, "w"), indent=2, sort_keys=True)
open(out_path, "a").write("\n")
print(f"SBOM: {len(kept)} components kept, {len(dropped)} not in CLI build graph dropped -> {out_path}",
      file=sys.stderr)
PY
