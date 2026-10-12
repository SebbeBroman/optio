//! Solver options. Mirrors the subset of HiGHS options the planning model sets,
//! plus an explicit parallelism knob.

#[derive(Clone, Debug)]
pub struct Options {
    pub output_flag: bool,
    pub time_limit_secs: f64,
    pub mip_rel_gap: f64,
    pub mip_abs_gap: f64,
    /// Native worker count. Ignored on wasm32 (single-threaded there).
    pub threads: usize,
    pub seed: u64,
    pub presolve: bool,
    /// Reoptimize child relaxations from the parent basis, with cold fallback.
    pub lp_warm_start: bool,
    /// Root integer strengthening, objective-domain bounds, and a small cut pool.
    pub mip_cuts: bool,
    /// Learn per-worker branching degradation estimates.
    pub mip_pseudocost: bool,
    /// Propagate implied bounds after branching, before solving the child LP.
    pub mip_node_presolve: bool,
    /// Replace identical bounded columns by a sum variable and lift witnesses.
    pub mip_column_compression: bool,
    /// Expose bounded integer objective-group counts to branching.
    pub mip_objective_counts: bool,
    /// Round certified bounds to a superset of attainable integer objectives.
    pub mip_objective_lattice: bool,
    /// Probe unreliable branching candidates near the root.
    pub mip_strong_branch: bool,
    /// Optional search resource limit; the default relies on the time limit.
    pub mip_max_nodes: u64,
    /// Opt-in early local-search stopping; disabled by default to preserve quality.
    pub adaptive_search: bool,
    /// Stop early once ring loads are within this fraction of the session mean.
    pub balance_target_ratio: f64,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            output_flag: true,
            time_limit_secs: 20.0,
            mip_rel_gap: 1e-6,
            mip_abs_gap: 0.0,
            threads: 1,
            seed: 0,
            presolve: true,
            lp_warm_start: true,
            mip_cuts: true,
            mip_pseudocost: true,
            mip_node_presolve: true,
            mip_column_compression: false,
            mip_objective_counts: true,
            mip_objective_lattice: true,
            mip_strong_branch: true,
            mip_max_nodes: u64::MAX,
            adaptive_search: false,
            balance_target_ratio: 0.15,
        }
    }
}
