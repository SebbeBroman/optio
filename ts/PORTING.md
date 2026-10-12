# optio-ts porting conventions

A faithful TypeScript port of the Rust `optio` LP/MIP engine
(`crates/core/src/*.rs`), tuned for **single-threaded V8 / browser** performance.
Module `src/foo.ts` mirrors `crates/core/src/foo.rs` (same algorithms, same
branch structure, same tolerances). Read the Rust file before porting; change
nothing about the maths.

## Rules

- **No `enum`**, no `namespace`, no decorators. Numeric enums are `as const`
  objects plus a derived type (see `status.ts`, `lp.ts`).
- **No `any`.** Use concrete types; `unknown` only with a checked cast.
- **No `require`/dynamic import.** Static ESM imports with explicit `.ts`
  extensions: `import { Model } from "./model.ts";`.
- Deterministic: no `Math.random`, no wall-clock other than the injected
  `Clock`. Rust `HashMap` iteration order is not relied upon for results; if the
  Rust code sorts or folds, do the same.
- Faithful arithmetic: `f64::max`/`f64::min` ignore `NaN` — use `fmax`/`fmin`
  from `model.ts`, not `Math.max`/`Math.min`, wherever a `NaN` could appear.
- `usize::MAX` sentinel: use a `-1` sentinel when the Rust code tests `>=`/`<`
  against it; otherwise a large sentinel constant local to the file.

## Performance posture (single-threaded)

- Hot fixed-size numeric/id vectors: `Float64Array` / `Int32Array`.
- Growing collections (`push`): plain `number[]` is fine and fast.
- Avoid allocating in inner loops; reuse scratch arrays/objects.
- Prefer two parallel arrays over arrays of `[number, number]` pairs in hot
  code. A `Vec<(u32,f64)>` sparse column can be an object/class with `idx`/`val`
  parallel arrays.
- `Rc`/`Arc` disappears: share the underlying array/object directly.

## Core API you build on (`src/model.ts`, `src/clock.ts`)

```ts
import { Model, INF, VAR_CONTINUOUS, VAR_INTEGER, VAR_BINARY, isIntegral,
         checkSolution, fmax, fmin, type SolutionCheck } from "./model.ts";
import type { Clock } from "./clock.ts";

// Model fields (all public): numCol, numRow, colCost, colLower, colUpper,
// colType, rowLower, rowUpper, aStart, aIndex, aValue (all number[] /
// VarType[]). Methods: numNz(), addCol(lo,hi): number,
// addRow(lo,hi,entries: readonly (readonly [number,number])[]): number,
// addCsr(...), setCost(col,cost), setIntegrality(col,type), objective(x),
// clone().
// checkSolution(model, x, tol) -> SolutionCheck { feasible, maxRowViolation,
//   maxBoundViolation, maxIntegralityViolation, objective }.

// Clock: `interface Clock { nowSecs(): number }`.
```

`VarType` is `0 | 1 | 2` (`VAR_CONTINUOUS`/`VAR_INTEGER`/`VAR_BINARY`).

## Public surface to expose (camelCase of the Rust `pub fn`)

- `block_planner.rs` -> `improve(model, start: number[] | null, clock, budget): number[] | null`,
  `rowBound(model): number | null`, `schedulingBound(model, clock, deadline): number | null`.
- `local_search.rs` -> `Repair { x: number[]; moves: number }`,
  `repair(model, x0, maxMoves, seed): Repair | null`,
  `constructInlinedPlanning(model): number[] | null`,
  `improvePlanning(model, x0, clock, deadlineSecs, seed): [number, number[]] | null`,
  `improvePlanningAdaptive(model, x0, clock, deadlineSecs, seed, adaptive): [number, number[]] | null`.

## Testing

- Add `test/<module>.test.ts` using `node:test` + `node:assert/strict`.
- Run `node --test test/<module>.test.ts` and `npx tsc --noEmit`.
- Ignore `tsc` errors that are not in your own file (other modules are being
  ported concurrently). Never edit files you were not asked to own.
