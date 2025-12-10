use std::cell::RefCell;
use z3::{ast::Bool, SatResult, Solver};
use z3::ast::Dynamic;
use crate::event_label::{ConstraintEval, ConstraintKind};
use crate::runtime::execution::ExecutionState;
use crate::TypeNondet;

mod expr;

pub use expr::*;

thread_local! {
    static FORMULAE: RefCell<Vec<StoredExpr>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn new_solver() -> Solver {
    Solver::new()
}

pub(crate) fn push_constraint(solver: &mut Solver, predicate: &Bool) {
    solver.assert(predicate);
}

pub(crate) fn is_sat(solver: &mut Solver) -> bool {
    solver.check() == SatResult::Sat
}

pub(crate) fn fork_with(solver: &Solver, predicate: &Bool) -> Option<Solver> {
    let child = Solver::new();
    for assertion in solver.get_assertions() {
        child.assert(&assertion);
    }
    child.assert(predicate);

    match child.check() {
        SatResult::Unsat => None,
        _ => Some(child),
    }
}

#[derive(Clone)]
struct StoredExpr {
    ast: Dynamic,
}

/// Store a formula in the thread-local registry and return its id.
pub(crate) fn store_formula(pred: Dynamic) -> usize {
    FORMULAE.with(|f| {
        let mut vec = f.borrow_mut();
        vec.push(StoredExpr { ast: pred });
        vec.len() - 1
    })
}

/// Retrieve a formula by id from the thread-local registry.
pub(crate) fn get_formula(id: usize) -> Option<Dynamic> {
    FORMULAE.with(|f| f.borrow().get(id).map(|s| s.ast.clone()))
}

/// Record a symbolic assume.
pub fn assume(sym: SymExpr) {
    let formula = ConstraintEval::new(
        ExecutionState::with(|s| s.next_pos()),
        Some(sym.id()),
        ConstraintKind::Assume,
        false,
    );
    ExecutionState::with(|s| s.must.borrow_mut().handle_constraint_eval(formula));
}

/// Record a symbolic assert.
pub fn assert(sym: SymExpr) {
    let formula = ConstraintEval::new(
        ExecutionState::with(|s| s.next_pos()),
        Some(sym.id()),
        ConstraintKind::Assert,
        false,
    );
    ExecutionState::with(|s| s.must.borrow_mut().handle_constraint_eval(formula));
}

/// Evaluate a symbolic boolean condition by exploring both branches.
/// The returned bool matches the branch taken and installs the corresponding
/// constraint in the solver.
pub fn eval(sym: SymExpr) -> bool {
    let branch = bool::nondet();
    if branch {
        assume(sym);
    } else {
        assume(sym.neg());
    }
    branch
}
