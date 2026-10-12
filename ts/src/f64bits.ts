// Fast f64 bit-level operations (Rust `f64::next_up`/`next_down`/`total_cmp`)
// without BigInt. The BigInt `DataView.getBigUint64` path allocates a BigInt on
// every call, which dominates `rowBound` (called per nonzero). Here we operate
// on the two 32-bit words of a shared Float64Array view instead.
//
// Little-endian word order (x86-64, arm64, wasm32) is assumed: u32[0] is the low
// word, u32[1] the high word.

const BUF = new ArrayBuffer(8);
const F = new Float64Array(BUF);
const U = new Uint32Array(BUF);

const SIGN = 0x80000000;
const INF_HI = 0x7ff00000;
const NEG_INF_HI = 0xfff00000;

/** Rust `f64::next_up`. */
export function nextUp(x: number): number {
  if (x !== x) return x;
  F[0] = x;
  let lo = U[0];
  let hi = U[1];
  if (hi === INF_HI && lo === 0) return x; // +Infinity
  if ((hi & 0x7fffffff) === 0 && lo === 0) {
    // ±0 -> MIN_VALUE
    U[0] = 1;
    U[1] = 0;
    return F[0];
  }
  if ((hi & SIGN) === 0) {
    lo = lo + 1;
    if (lo > 0xffffffff) {
      lo = 0;
      hi = hi + 1;
    }
  } else if (lo === 0) {
    lo = 0xffffffff;
    hi = hi - 1;
  } else {
    lo = lo - 1;
  }
  U[0] = lo;
  U[1] = hi;
  return F[0];
}

/** Rust `f64::next_down`. */
export function nextDown(x: number): number {
  if (x !== x) return x;
  F[0] = x;
  let lo = U[0];
  let hi = U[1];
  if (hi === NEG_INF_HI && lo === 0) return x; // -Infinity
  if ((hi & 0x7fffffff) === 0 && lo === 0) {
    // ±0 -> -MIN_VALUE
    U[0] = 1;
    U[1] = SIGN;
    return F[0];
  }
  if ((hi & SIGN) === 0) {
    if (lo === 0) {
      lo = 0xffffffff;
      hi = hi - 1;
    } else {
      lo = lo - 1;
    }
  } else {
    lo = lo + 1;
    if (lo > 0xffffffff) {
      lo = 0;
      hi = hi + 1;
    }
  }
  U[0] = lo;
  U[1] = hi;
  return F[0];
}

/**
 * Rust `f64::total_cmp`: a total order over every bit pattern.
 * `a ^= ((a >> 63) as u64 >> 1) as i64` then signed compare, on the two words.
 */
export function totalCmp(a: number, b: number): number {
  F[0] = a;
  let alo = U[0];
  let ahi = U[1];
  F[0] = b;
  let blo = U[0];
  let bhi = U[1];
  if ((ahi & SIGN) !== 0) {
    ahi = (ahi ^ 0x7fffffff) >>> 0;
    alo = (alo ^ 0xffffffff) >>> 0;
  }
  if ((bhi & SIGN) !== 0) {
    bhi = (bhi ^ 0x7fffffff) >>> 0;
    blo = (blo ^ 0xffffffff) >>> 0;
  }
  const ash = ahi | 0; // signed high word
  const bsh = bhi | 0;
  if (ash !== bsh) return ash < bsh ? -1 : 1;
  if (alo !== blo) return alo < blo ? -1 : 1;
  return 0;
}
