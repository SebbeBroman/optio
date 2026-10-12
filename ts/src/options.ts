// Solver options. Mirrors the subset of HiGHS options the planning model sets,
// plus an explicit parallelism knob. (Port of `crates/core/src/options.rs`.)
//
// The port is single-threaded: `threads > 1` selects the serial path (the Rust
// `parallel` feature is native-only and compiled out for the wasm target).

export interface Options {
  outputFlag: boolean;
  timeLimitSecs: number;
  mipRelGap: number;
  mipAbsGap: number;
  /** Native worker count. The TS port always runs the serial path. */
  threads: number;
  seed: number;
  presolve: boolean;
  /** Reoptimize child relaxations from the parent basis, with cold fallback. */
  lpWarmStart: boolean;
  /** Root integer strengthening, objective-domain bounds, and a small cut pool. */
  mipCuts: boolean;
  /** Learn per-worker branching degradation estimates. */
  mipPseudocost: boolean;
  /** Opt-in early local-search stopping; disabled by default to preserve quality. */
  adaptiveSearch: boolean;
  /** Stop early once ring loads are within this fraction of the session mean. */
  balanceTargetRatio: number;
}

export function defaultOptions(): Options {
  return {
    outputFlag: true,
    timeLimitSecs: 20.0,
    mipRelGap: 1e-6,
    mipAbsGap: 0.0,
    threads: 1,
    seed: 0,
    presolve: true,
    lpWarmStart: true,
    mipCuts: true,
    mipPseudocost: true,
    adaptiveSearch: false,
    balanceTargetRatio: 0.15,
  };
}
