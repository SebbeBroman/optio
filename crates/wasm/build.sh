#!/usr/bin/env bash
# Build the optio wasm package.
#
# Env:
#   WASM_BINDGEN_TARGET  wasm-bindgen output target: `nodejs` (default) or `web`.
#   PKG_DIR              output directory (default: <root>/pkg).
#   OPTIO_SMALL_ALLOC    1 selects the optional smaller free-list allocator.
#
# The nodejs target produces a CommonJS loader for the Node harness; the web
# target produces an ES module with an async `init()` for bundlers (Vite).
#
# Config comes from the workspace:
#   * Cargo.toml   [profile.release-small]  opt-level="z", lto="fat",
#                                           codegen-units=1, panic="abort"
#   * .cargo/config.toml  [target.wasm32-unknown-unknown] rustflags
#                         "-C target-feature=+simd128"  (native builds unaffected)
#
# Set OPTIO_NO_WASM_OPT=1 to skip the post-link `wasm-opt -Oz` size pass.
#
# The wasm build remaps $HOME out of embedded paths (`--remap-path-prefix`)
# so panic Location strings do not carry the build machine's layout into the
# published binary.
#
# Size: when a nightly toolchain with `rust-src` is available, the build uses
# `-Z build-std=std,panic_abort` with `-C panic=immediate-abort`, which drops
# panic machinery. Current measured package sizes are recorded in the
# performance reports. Set OPTIO_STABLE=1 to force the stable path.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

target="${WASM_BINDGEN_TARGET:-nodejs}"
pkg_dir="${PKG_DIR:-$root/pkg}"
mkdir -p "$pkg_dir"
allocator_args=(--features default-alloc)
if [[ "${OPTIO_SMALL_ALLOC:-0}" == "1" ]]; then
  allocator_args=(--no-default-features)
fi

rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true

use_nightly=0
if [[ "${OPTIO_STABLE:-0}" != "1" ]] \
  && cargo +nightly --version >/dev/null 2>&1 \
  && rustup component list --toolchain nightly 2>/dev/null | grep -q 'rust-src (installed)'; then
  use_nightly=1
fi

if [[ "$use_nightly" == "1" ]]; then
  echo "build: nightly + build-std + panic=immediate-abort"
  RUSTFLAGS="-C target-feature=+simd128 -Zunstable-options -Cpanic=immediate-abort --remap-path-prefix=$HOME=" \
    cargo +nightly build -p optio-wasm --target wasm32-unknown-unknown \
      --profile release-small "${allocator_args[@]}" \
      -Z build-std=std,panic_abort
else
  echo "build: stable (set up nightly + rust-src for the smaller immediate-abort build)"
  # RUSTFLAGS replaces [target.wasm32-unknown-unknown] rustflags from
  # .cargo/config.toml, so simd128 must be repeated here. The remap keeps
  # $HOME-derived paths (rustup/cargo registry locations embedded in panic
  # Location strings) out of the shipped .wasm.
  RUSTFLAGS="--remap-path-prefix=$HOME= -C target-feature=+simd128" \
    cargo build -p optio-wasm --target wasm32-unknown-unknown --profile release-small "${allocator_args[@]}"
fi

wasm-bindgen \
  --target "$target" \
  --out-dir "$pkg_dir" \
  "$root/target/wasm32-unknown-unknown/release-small/optio_wasm.wasm"

# Post-link size pass. The `--enable-*` set is exactly the features the
# wasm32-unknown-unknown target turns on by default (plus simd128 from
# .cargo/config.toml); we enable them explicitly rather than using wasm-opt's
# `-all`, because `-all` also turns on experimental proposals
# (custom-descriptors / exact references) whose output Node/V8 refuses to
# compile. wasm-bindgen has already emitted the JS from the un-stripped module
# above, so this only shrinks the .wasm. See harness/tools/wasm-size.sh.
if [[ "${OPTIO_NO_WASM_OPT:-0}" != "1" ]] && command -v wasm-opt >/dev/null 2>&1; then
  wasm-opt \
    --enable-simd --enable-bulk-memory --enable-nontrapping-float-to-int \
    --enable-mutable-globals --enable-reference-types --enable-sign-ext \
    --enable-multivalue \
    -Oz --strip-debug --vacuum --converge \
    "$pkg_dir/optio_wasm_bg.wasm" -o "$pkg_dir/optio_wasm_bg.wasm"
fi

if [[ "$target" == "nodejs" ]]; then
  # The parent directory has a "type":"module" package.json, so mark the
  # emitted CJS loader as CommonJS (it uses __dirname/require for the bytes).
  printf '{"type":"commonjs"}\n' > "$pkg_dir/package.json"
fi

echo "built ($target): $pkg_dir/optio_wasm.js"
ls -la "$pkg_dir"
