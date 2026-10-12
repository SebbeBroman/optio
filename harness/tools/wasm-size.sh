#!/usr/bin/env bash
# wasm-size.sh — build one or more optio-wasm variants and report artifact size.
#
# For each variant it:
#   1. runs `cargo build -p optio-wasm --target wasm32-unknown-unknown` with a
#      chosen profile + RUSTFLAGS (RUSTFLAGS overrides the [target.*] rustflags
#      in .cargo/config.toml, so each variant is fully explicit),
#   2. runs `wasm-bindgen --target nodejs` into the variant's output dir,
#   3. optionally runs `wasm-opt` in place on the emitted .wasm,
#   4. prints raw / gzip -9 / brotli -11 bytes for both the .wasm and the JS
#      loader.
#
# Usage:
#   harness/tools/wasm-size.sh [preset ...]        # build the named presets
#   harness/tools/wasm-size.sh --matrix            # all presets, one table
#   harness/tools/wasm-size.sh --list              # list preset names
#
# Presets (each isolates one config axis):
#   base          no SIMD, no strip,                   wasm-opt -Oz
#   base-noopt    no SIMD, no strip,                   (no wasm-opt)
#   simd          +simd128, no strip,                  wasm-opt -Oz --strip-debug --vacuum --converge   <-- recommended
#   simd-noopt    +simd128, no strip,                  (no wasm-opt)
#   cargo-strip   +simd128, -C strip=symbols,          wasm-opt -Oz --strip-debug --vacuum --converge
#   nightly       nightly build-std=std,panic_abort, +simd128,  full wasm-opt
#   nightly-imm   nightly build-std + panic=immediate-abort, +simd128,  full wasm-opt   <-- leanest (opt-in)
#   nightly-imm-noopt  same as nightly-imm but without wasm-opt
#
# Custom single variant (all optional):
#   harness/tools/wasm-size.sh --name foo --profile release-small \
#       --rustflags "-C target-feature=+simd128" \
#       --wasm-opt "-Oz --strip-debug" --out target/wasm-size/foo
#   ... --no-simd, --no-wasm-opt, --build-std, --panic-immediate-abort
#
# Options:
#   -o, --out DIR      root output dir (default: target/wasm-size)
#       --run-bench    after the last build, run
#                      `node harness/tools/wasm-bench.mjs lp/lp-11.lp`
#                      against that variant's package (sanity/smoke check)
#
# Caveats:
#   * SIMD (`simd128`) needs a runtime with WebAssembly SIMD (all modern
#     browsers; Node >= 16). `base`/`base-noopt` build without it.
#   * `nightly*` presets need the nightly toolchain and its `rust-src`
#     component (`rustup component add rust-src --toolchain nightly`).
#     They are opt-in; the default stable build never uses build-std.
#   * `panic=immediate-abort` drops panic messages (behavioral change: an
#     abort no longer reports a message) and also needs nightly — hence
#     opt-in.
#
# The script never touches ../pkg; crates/wasm/build.sh owns that. Outputs land
# under target/wasm-size/<preset>/ (gitignored via /target).

set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

OUT_ROOT="$root/target/wasm-size"
RUN_BENCH=0
CUSTOM=0
EXTRA_RUSTFLAGS=""
STD_PANIC_FLAGS="-Zunstable-options -Cpanic=immediate-abort"
declare -a PRESETS=()
declare -a ROWS=()

# ---- helpers ---------------------------------------------------------------

die() { echo "wasm-size: $*" >&2; exit 1; }

have() { command -v "$1" >/dev/null 2>&1; }

# size3 FILE -> "raw gzip brotli"
size3() {
  local f="$1" raw gz br
  raw=$(wc -c <"$f" | tr -d ' ')
  gz=$(gzip -9 -c "$f" | wc -c | tr -d ' ')
  br=$(brotli -q 11 -c "$f" | wc -c | tr -d ' ')
  echo "$raw $gz $br"
}

human() { # bytes -> comma-grouped
  printf '%s' "$1" | sed -e :a -e 's/\(.*[0-9]\)\([0-9][0-9][0-9]\)/\1,\2/;ta'
}

# build_one NAME PROFILE RUSTFLAGS BUILD_STD BUILD_STD_FEATURES WASM_OPT
build_one() {
  local name="$1" profile="$2" rflags="$3" bstd="$4" bstd_features="$5" wopt="$6"
  local out="$OUT_ROOT/$name"
  local wasm="target/wasm32-unknown-unknown/$profile/optio_wasm.wasm"

  mkdir -p "$out"
  rm -f "$out"/optio_wasm*.wasm "$out"/optio_wasm*.js "$out"/optio_wasm*.d.ts "$out"/package.json

  local -a cargo=(cargo)
  local -a args=(build -p optio-wasm --target wasm32-unknown-unknown --profile "$profile")
  if [[ -n "$bstd" || -n "$bstd_features" ]]; then
    cargo=(cargo +nightly)
    [[ -n "$bstd" ]] && args+=(-Z "build-std=$bstd")
    [[ -n "$bstd_features" ]] && args+=(-Z "build-std-features=$bstd_features")
  fi

  local full_rflags="${rflags}${EXTRA_RUSTFLAGS:+ $EXTRA_RUSTFLAGS}"
  echo "==> [$name] ${cargo[*]} ${args[*]}  (RUSTFLAGS='$full_rflags')" >&2
  RUSTFLAGS="$full_rflags" "${cargo[@]}" "${args[@]}"

  [[ -f "$wasm" ]] || die "[$name] expected $wasm after build"
  wasm-bindgen --target nodejs --out-dir "$out" "$wasm"
  printf '{"type":"commonjs"}\n' >"$out/package.json"

  if [[ -n "$wopt" ]]; then
    have wasm-opt || die "wasm-opt requested for [$name] but not found"
    # shellcheck disable=SC2086  # $WASM_FEATURES/$wopt are intentionally word-split
    wasm-opt $WASM_FEATURES $wopt "$out/optio_wasm_bg.wasm" -o "$out/optio_wasm_bg.wasm"
  fi

  local w_raw w_gz w_br j_raw j_gz j_br
  read -r w_raw w_gz w_br <<<"$(size3 "$out/optio_wasm_bg.wasm")"
  read -r j_raw j_gz j_br <<<"$(size3 "$out/optio_wasm.js")"
  ROWS+=("$name|$wopt|$w_raw|$w_gz|$w_br|$j_raw|$j_gz|$j_br|$out")
  LAST_OUT="$out"
}

FULL_OPT="-Oz --strip-debug --vacuum --converge"
# Exactly the features the wasm32-unknown-unknown target enables by default,
# plus simd128. Deliberately not wasm-opt's `-all`: that also enables
# experimental custom-descriptors / exact references, and the emitted module
# then fails to compile in Node/V8 ("invalid heap type 'exact'").
WASM_FEATURES="--enable-simd --enable-bulk-memory --enable-nontrapping-float-to-int --enable-mutable-globals --enable-reference-types --enable-sign-ext --enable-multivalue"

list_presets() {
  cat <<'EOF'
base
base-noopt
simd
simd-noopt
cargo-strip
nightly
nightly-imm
nightly-imm-noopt
EOF
}

run_preset() {
  local p="$1"
  case "$p" in
    base)
      build_one "base" "release-small" "-C target-feature=-simd128" "" "" "-Oz" ;;
    base-noopt)
      build_one "base-noopt" "release-small" "-C target-feature=-simd128" "" "" "" ;;
    simd)
      build_one "simd" "release-small" "-C target-feature=+simd128" "" "" "$FULL_OPT" ;;
    simd-noopt)
      build_one "simd-noopt" "release-small" "-C target-feature=+simd128" "" "" "" ;;
    cargo-strip)
      build_one "cargo-strip" "release-small" "-C target-feature=+simd128 -C strip=symbols" "" "" "$FULL_OPT" ;;
    nightly)
      build_one "nightly" "release-small" "-C target-feature=+simd128" \
        "std,panic_abort" "" "$FULL_OPT" ;;
    nightly-imm)
      build_one "nightly-imm" "release-small" "-C target-feature=+simd128 $STD_PANIC_FLAGS" \
        "std,panic_abort" "" "$FULL_OPT" ;;
    nightly-imm-noopt)
      build_one "nightly-imm-noopt" "release-small" "-C target-feature=+simd128 $STD_PANIC_FLAGS" \
        "std,panic_abort" "" "" ;;
    *)
      die "unknown preset '$p' (see --list)" ;;
  esac
}

# ---- arg parsing -----------------------------------------------------------

if [[ $# -eq 0 ]]; then
  # stable-only default; add `nightly-imm` explicitly (or use --matrix) to
  # build the leanest variant, which needs the nightly toolchain + rust-src.
  set -- base simd
fi
while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help) sed -n '2,50p' "$0"; exit 0 ;;
    --list) list_presets; exit 0 ;;
    --matrix) PRESETS=(base base-noopt simd simd-noopt cargo-strip nightly nightly-imm nightly-imm-noopt); shift ;;
    -o|--out) OUT_ROOT="$2"; shift 2 ;;
    --run-bench) RUN_BENCH=1; shift ;;
    --name) CUSTOM=1; CUSTOM_NAME="$2"; shift 2 ;;
    --profile) CUSTOM=1; CUSTOM_PROFILE="$2"; shift 2 ;;
    --rustflags) CUSTOM=1; CUSTOM_RFLAGS="$2"; shift 2 ;;
    --wasm-opt) CUSTOM=1; CUSTOM_WOPT="$2"; shift 2 ;;
    --no-simd) CUSTOM=1; SIMD=0; shift ;;
    --no-wasm-opt) CUSTOM=1; CUSTOM_WOPT=""; shift ;;
    --build-std) CUSTOM=1; BSTD="std,panic_abort"; shift ;;
    --panic-immediate-abort) CUSTOM=1; BSTD="std,panic_abort"; IMM=1; shift ;;
    --*) die "unknown option '$1' (try --help)" ;;
    *) PRESETS+=("$1"); shift ;;
  esac
done

if [[ "$CUSTOM" -eq 1 ]]; then
  : "${CUSTOM_NAME:=custom}"
  : "${CUSTOM_PROFILE:=release-small}"
  : "${CUSTOM_WOPT:=$FULL_OPT}"
  SIMD="${SIMD:-1}"
  declare -a flags=()
  [[ "$SIMD" -eq 1 ]] && flags+=("-C" "target-feature=+simd128")
  [[ "${IMM:-0}" -eq 1 ]] && flags+=($STD_PANIC_FLAGS)
  if [[ -n "${CUSTOM_RFLAGS:-}" ]]; then
    # explicit rustflags replace the auto-derived simd selection
    flags=("$CUSTOM_RFLAGS")
    [[ "${IMM:-0}" -eq 1 ]] && flags+=($STD_PANIC_FLAGS)
  fi
  build_one "${CUSTOM_NAME}" "${CUSTOM_PROFILE}" "${flags[*]}" "${BSTD:-}" "" "${CUSTOM_WOPT}"
fi

if [[ ${#PRESETS[@]} -gt 0 ]]; then
  for p in "${PRESETS[@]}"; do
    run_preset "$p"
  done
fi

# ---- report ----------------------------------------------------------------

echo
printf '%-13s %-37s %9s %9s %9s %9s %8s %8s\n' \
  "variant" "wasm-opt" "wasm_raw" "wasm_gz" "wasm_br" "js_raw" "js_gz" "js_br"
printf -- '-%.0s' {1..112}
echo
for row in ${ROWS[@]+"${ROWS[@]}"}; do
  IFS='|' read -r name wopt w_raw w_gz w_br j_raw j_gz j_br _out <<<"$row"
  printf '%-13s %-37s %9s %9s %9s %9s %8s %8s\n' \
    "$name" "${wopt:--}" "$(human "$w_raw")" "$(human "$w_gz")" "$(human "$w_br")" \
    "$(human "$j_raw")" "$(human "$j_gz")" "$(human "$j_br")"
done
echo
echo "artifacts under: $OUT_ROOT"

if [[ "$RUN_BENCH" -eq 1 && -n "${LAST_OUT:-}" ]]; then
  echo
  echo "==> smoke test: node harness/tools/wasm-bench.mjs lp/lp-11.lp  (LAST_OUT=$LAST_OUT)" >&2
  # Temporarily expose the variant as pkg/ for optio.mjs (which requires pkg/),
  # then restore whatever was there so build.sh's pkg/ is untouched.
  pkg="$root/pkg"
  backup="$root/target/wasm-size/.pkg-backup"
  had_pkg=0
  if [[ -e "$pkg" ]]; then had_pkg=1; rm -rf "$backup"; cp -R "$pkg" "$backup"; fi
  rm -rf "$pkg"; cp -R "$LAST_OUT" "$pkg"
  set +e
  node harness/tools/wasm-bench.mjs lp/lp-11.lp
  rc=$?
  set -e
  rm -rf "$pkg"
  if [[ "$had_pkg" -eq 1 ]]; then mv "$backup" "$pkg"; else rm -rf "$backup"; fi
  exit "$rc"
fi
