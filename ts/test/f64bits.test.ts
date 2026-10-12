// Validate the fast f64bit helpers against a BigInt reference + known values.
import { test } from "node:test";
import assert from "node:assert/strict";
import { nextUp, nextDown, totalCmp } from "../src/f64bits.ts";

const MASK = (1n << 64n) - 1n;
const buf = new ArrayBuffer(8);
const f = new Float64Array(buf);
const dv = new DataView(buf);
function toBits(x: number): bigint {
  f[0] = x;
  return dv.getBigUint64(0, true);
}
function fromBits(b: bigint): number {
  dv.setBigUint64(0, BigInt.asUintN(64, b), true);
  return f[0];
}
const SIGN64 = 1n << 63n;
const MASK63 = SIGN64 - 1n;
const TWO64 = 1n << 64n;
function refOrderKey(v: number): bigint {
  const bits = toBits(v);
  const key = (bits & SIGN64) !== 0n ? bits ^ MASK63 : bits;
  return key >= SIGN64 ? key - TWO64 : key;
}

test("nextUp/nextDown match a BigInt reference", () => {
  let s = 12345n;
  const rnd = () => {
    s = (s * 6364136223846793005n + 1n) & MASK;
    return Number(s >> 11n) / 9007199254740992;
  };
  const vals = [0, -0, 1, -1, 1e-300, -1e-300, 1e300, -1e300, 5e-324, Infinity, -Infinity, NaN];
  for (let i = 0; i < 2000; i++) vals.push((rnd() - 0.5) * Math.pow(10, Math.floor(rnd() * 40) - 20));
  for (const v of vals) {
    const b = toBits(v);
    // reference next_up
    let refUp: bigint;
    if (v !== v || b === toBits(Infinity)) refUp = b;
    else {
      const abs = b & MASK63;
      refUp = abs === 0n ? 1n : b === abs ? b + 1n : b - 1n;
    }
    let refDown: bigint;
    if (v !== v || b === toBits(-Infinity)) refDown = b;
    else {
      const abs = b & MASK63;
      refDown = abs === 0n ? 0x8000000000000001n : b === abs ? b - 1n : b + 1n;
    }
    assert.equal(toBits(nextUp(v)), refUp, `nextUp(${v})`);
    assert.equal(toBits(nextDown(v)), refDown, `nextDown(${v})`);
  }
});

test("totalCmp matches the BigInt reference ordering", () => {
  let s = 999n;
  const rnd = () => {
    s = (s * 6364136223846793005n + 1n) & MASK;
    return Number(s >> 11n) / 9007199254740992;
  };
  const vals = [0, -0, 1, -1, Infinity, -Infinity, NaN, Number.MAX_VALUE, -Number.MAX_VALUE];
  for (let i = 0; i < 500; i++) vals.push((rnd() - 0.5) * 2 ** (Math.floor(rnd() * 60) - 30));
  for (const a of vals) for (const b of vals) {
    const ka = refOrderKey(a);
    const kb = refOrderKey(b);
    const ref = ka < kb ? -1 : ka > kb ? 1 : 0;
    assert.equal(totalCmp(a, b), ref, `totalCmp(${a},${b})`);
  }
});
