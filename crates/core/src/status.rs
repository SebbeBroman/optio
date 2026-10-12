//! Model status codes, numbered to match the HiGHS / `highs-js` `ModelStatus`
//! enum so a differential harness can compare them directly.

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(i32)]
pub enum ModelStatus {
    #[default]
    NotSet = 0,
    LoadError = 1,
    ModelError = 2,
    PresolveError = 3,
    SolveError = 4,
    PostsolveError = 5,
    Empty = 6,
    Optimal = 7,
    Infeasible = 8,
    UnboundedOrInfeasible = 9,
    Unbounded = 10,
    ObjectiveBound = 11,
    ObjectiveTarget = 12,
    TimeLimit = 13,
    IterationLimit = 14,
    Unknown = 15,
    SolutionLimit = 16,
    Interrupt = 17,
}

impl ModelStatus {
    #[inline]
    pub fn as_i32(self) -> i32 {
        self as i32
    }

    pub fn is_infeasible(self) -> bool {
        matches!(
            self,
            ModelStatus::Infeasible | ModelStatus::UnboundedOrInfeasible
        )
    }

    pub fn has_feasible_solution(self) -> bool {
        matches!(
            self,
            ModelStatus::Optimal
                | ModelStatus::TimeLimit
                | ModelStatus::IterationLimit
                | ModelStatus::ObjectiveBound
                | ModelStatus::ObjectiveTarget
                | ModelStatus::SolutionLimit
                | ModelStatus::Interrupt
        )
    }
}
