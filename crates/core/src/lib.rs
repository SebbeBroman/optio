//! `optio-core`: a data-oriented, wasm-first LP/MIP core inspired by HiGHS.
//!
//! Design rules (see `PLAN.md`):
//! - Flat, index-based storage; no per-variable objects, no owning pointers.
//! - The model is the shared, immutable input; all mutable state lives in
//!   per-solve contexts so native code can parallelise without locks.
//! - `check_solution` is the oracle; the solver's own status is never trusted.

mod block_planner;
mod column_compression;
pub mod heuristic;
pub mod local_search;
pub mod lp;
pub mod mip;
pub mod model;
mod objective_counts;
pub mod options;
mod presolve;
pub mod solve;
pub mod status;

pub use model::{check_solution, Model, RowId, SolutionCheck, VarId, VarType, INF};
pub use options::Options;
pub use solve::{
    initial_point, solve, solve_with, solve_with_start, Callbacks, Clock, Incumbent, Progress,
    Solution, StdClock,
};
pub use status::ModelStatus;

pub mod prelude {
    pub use crate::model::{Model, RowId, VarId, VarType, INF};
    pub use crate::options::Options;
    pub use crate::solve::{solve, solve_with, StdClock};
    pub use crate::status::ModelStatus;
}
