use serde::{Deserialize, Serialize};
use z3::{ast::Bool, SatResult, Solver};
use std::cell::RefCell;

use crate::event_label::{ConstraintEval, ConstraintKind};
use crate::runtime::execution::ExecutionState;

thread_local! {
    static FORMULAE: RefCell<Vec<StoredBool>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn new_solver() -> Solver {
    Solver::new()
}

pub(crate) fn fresh_bool(name_hint: impl AsRef<str>) -> Bool {
    Bool::fresh_const(name_hint.as_ref())
}

pub(crate) fn push_constraint(solver: &mut Solver, predicate: &Bool) {
    solver.assert(predicate);
}

pub(crate) fn is_sat(solver: &mut Solver) -> bool {
    solver.check() == SatResult::Sat
}

pub(crate) fn fork_with(
    solver: &Solver,
    predicate: &Bool,
) -> Option<Solver> {
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
struct StoredBool {
    ast: Bool,
}

/// Store a formula in the thread-local registry and return its id.
pub(crate) fn store_formula(pred: Bool) -> usize {
    FORMULAE.with(|f| {
        let mut vec = f.borrow_mut();
        vec.push(StoredBool { ast: pred });
        vec.len() - 1
    })
}

/// Retrieve a formula by id from the thread-local registry.
pub(crate) fn get_formula(id: usize) -> Option<Bool> {
    FORMULAE.with(|f| f.borrow().get(id).map(|s| s.ast.clone()))
}

/// An identifier for a stored symbolic boolean.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct SymBool {
    id: usize,
}

impl SymBool {
    pub fn from_id(id: usize) -> Self {
        SymBool { id }
    }

    pub fn neg(self) -> Self {
        let formula = get_formula(self.id).expect("unknown formula id");
        let neg = formula.not();
        let id = store_formula(neg);
        SymBool { id }
    }

    pub fn eq(self, other: SymBool) -> Self {
        let f1 = get_formula(self.id).expect("unknown formula id");
        let f2 = get_formula(other.id).expect("unknown formula id");
        let eq = f1.eq(&f2);
        let id = store_formula(eq);
        SymBool { id }
    }

    pub fn id(&self) -> usize {
        self.id
    }
}

/// Create a fresh symbolic boolean and return its id wrapper.
pub fn fresh_bool_symbol() -> SymBool {
    let f = fresh_bool("symb");
    SymBool {
        id: store_formula(f),
    }
}

/// Record a symbolic assume.
pub fn assume_symbolic(sym: SymBool) {
    let formula = ConstraintEval::new(
        ExecutionState::with(|s| s.next_pos()),
        Some(sym.id()),
        ConstraintKind::Assume,
        false,
    );
    ExecutionState::with(|s| s.must.borrow_mut().handle_constraint_eval(formula));
}

/// Record a symbolic assert.
pub fn assert_symbolic(sym: SymBool) {
    let formula = ConstraintEval::new(
        ExecutionState::with(|s| s.next_pos()),
        Some(sym.id()),
        ConstraintKind::Assert,
        false,
    );
    ExecutionState::with(|s| s.must.borrow_mut().handle_constraint_eval(formula));
}
