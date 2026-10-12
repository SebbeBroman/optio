// A hash map keyed by an exact `number[]` vector, with no key-string
// allocation. Replaces the `Map<string, ...>` + `join(",")` keys in the
// scheduling/day dynamic programs (Rust used `HashMap<Vec<u64>, _>`).
//
// Exactness: keys are compared with `===` per element. That matches the string
// form's equivalence for the finite values here (notably it treats `-0` and `0`
// as equal, as `String` does) and never sees `NaN`.

const HB = new ArrayBuffer(8);
const HF = new Float64Array(HB);
const HU = new Uint32Array(HB);

export class VecMap<V> {
  private buckets: Map<number, number[]> = new Map();
  private keys: number[][] = [];
  private vals: V[] = [];

  size(): number {
    return this.keys.length;
  }
  key(id: number): number[] {
    return this.keys[id];
  }
  val(id: number): V {
    return this.vals[id];
  }
  setVal(id: number, v: V): void {
    this.vals[id] = v;
  }

  private hashKey(v: readonly number[]): number {
    let h = 2166136261;
    for (let i = 0; i < v.length; i++) {
      HF[0] = v[i];
      h = Math.imul(h ^ HU[0], 16777619);
      h = Math.imul(h ^ HU[1], 16777619);
    }
    return h >>> 0;
  }
  private eq(a: readonly number[], b: readonly number[]): boolean {
    if (a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) {
      if (a[i] !== b[i]) return false;
    }
    return true;
  }
  /** Entry id for `key`, or -1. */
  find(key: readonly number[]): number {
    const b = this.buckets.get(this.hashKey(key));
    if (b === undefined) return -1;
    for (let t = 0; t < b.length; t++) {
      const id = b[t];
      if (this.eq(this.keys[id], key)) return id;
    }
    return -1;
  }
  /** Insert a new entry (the caller has already checked `find`). */
  insert(key: number[], value: V): number {
    const h = this.hashKey(key);
    let b = this.buckets.get(h);
    if (b === undefined) {
      b = [];
      this.buckets.set(h, b);
    }
    const id = this.keys.length;
    this.keys.push(key);
    this.vals.push(value);
    b.push(id);
    return id;
  }
}
