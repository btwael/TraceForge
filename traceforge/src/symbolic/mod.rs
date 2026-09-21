mod expr;
mod solver;

pub use expr::*;
pub(crate) use solver::SymbolicSolver;

use crate::event_label::{ConstraintEval, ConstraintKind, SymbolicVar};
use crate::runtime::execution::ExecutionState;

pub fn fresh_int() -> SymExpr {
    fresh(SymSort::Int)
}

pub fn fresh_bool() -> SymExpr {
    fresh(SymSort::Bool)
}

pub fn fresh_uninterpreted(name: impl Into<String>) -> SymExpr {
    fresh(SymSort::Uninterpreted(name.into()))
}

pub fn fresh(sort: SymSort) -> SymExpr {
    ExecutionState::with(|state| {
        let pos = state.next_pos();
        let id = SymVarId::from_event(pos);
        let owner = state.current().summarizable_call().cloned();
        state
            .must
            .borrow_mut()
            .handle_symbolic_var(SymbolicVar::new(pos, id.clone(), sort.clone(), owner));
        SymExpr::Var { id, sort }
    })
}

pub fn eval(sym_expr: SymExpr) -> bool {
    ExecutionState::with(|state| {
        let pos = state.next_pos();
        let owner = state.current().summarizable_call().cloned();
        let lab = ConstraintEval::new(pos, sym_expr, true, ConstraintKind::Branch, owner);
        state.must.borrow_mut().handle_constraint_eval(lab)
    })
}

pub fn assume(sym_expr: SymExpr) {
    let ok = eval(sym_expr);
    crate::assume_impl(ok, None);
}

pub fn assert(sym_expr: SymExpr) {
    let ok = eval(sym_expr);
    crate::assert(ok);
}
