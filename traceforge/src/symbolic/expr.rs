use super::{get_formula, store_formula};
use dyn_eq::DynEq;
use serde::{Deserialize, Serialize};
use std::ops::{Add, Div, Mul, Rem, Sub};
use z3::ast::{Bool, Dynamic, Int};

/// An identifier for a stored symbolic boolean.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SymExpr {
    pub id: usize,
}

impl SymExpr {
    pub fn from_id(id: usize) -> Self {
        SymExpr { id }
    }

    pub fn and(self, other: SymExpr) -> Self {
        let f1 = get_formula(self.id)
            .expect("unknown formula id")
            .as_bool()
            .unwrap();
        let f2 = get_formula(other.id)
            .expect("unknown formula id")
            .as_bool()
            .unwrap();
        let f = Bool::and(&[f1, f2]);
        let id = store_formula(Dynamic::from_ast(&f));
        SymExpr { id }
    }

    pub fn or(self, other: SymExpr) -> Self {
        let f1 = get_formula(self.id)
            .expect("unknown formula id")
            .as_bool()
            .unwrap();
        let f2 = get_formula(other.id)
            .expect("unknown formula id")
            .as_bool()
            .unwrap();
        let f = Bool::or(&[f1, f2]);
        let id = store_formula(Dynamic::from_ast(&f));
        SymExpr { id }
    }

    pub fn xor(self, other: SymExpr) -> Self {
        let f1 = get_formula(self.id)
            .expect("unknown formula id")
            .as_bool()
            .unwrap();
        let f2 = get_formula(other.id)
            .expect("unknown formula id")
            .as_bool()
            .unwrap();
        let f = f1.xor(f2);
        let id = store_formula(Dynamic::from_ast(&f));
        SymExpr { id }
    }

    pub fn implies(self, other: SymExpr) -> Self {
        let f1 = get_formula(self.id)
            .expect("unknown formula id")
            .as_bool()
            .unwrap();
        let f2 = get_formula(other.id)
            .expect("unknown formula id")
            .as_bool()
            .unwrap();
        let f = f1.implies(f2);
        let id = store_formula(Dynamic::from_ast(&f));
        SymExpr { id }
    }

    pub fn neg(self) -> Self {
        let formula = get_formula(self.id).expect("unknown formula id");
        let neg = formula.as_bool().unwrap().not();
        let id = store_formula(Dynamic::from_ast(&neg));
        SymExpr { id }
    }

    pub fn eq(self, other: SymExpr) -> Self {
        let f1 = get_formula(self.id).expect("unknown formula id");
        let f2 = get_formula(other.id).expect("unknown formula id");
        let eq = f1.eq(&f2);
        let id = store_formula(Dynamic::from_ast(&eq));
        SymExpr { id }
    }

    pub fn ge(self, other: SymExpr) -> Self {
        let f1 = get_formula(self.id).expect("unknown formula id");
        let f2 = get_formula(other.id).expect("unknown formula id");
        let ge = f1.as_int().unwrap().ge(&f2.as_int().unwrap());
        let id = store_formula(Dynamic::from_ast(&ge));
        SymExpr { id }
    }

    pub fn gt(self, other: SymExpr) -> Self {
        let f1 = get_formula(self.id).expect("unknown formula id");
        let f2 = get_formula(other.id).expect("unknown formula id");
        let gt = f1.as_int().unwrap().gt(&f2.as_int().unwrap());
        let id = store_formula(Dynamic::from_ast(&gt));
        SymExpr { id }
    }

    pub fn le(self, other: SymExpr) -> Self {
        let f1 = get_formula(self.id).expect("unknown formula id");
        let f2 = get_formula(other.id).expect("unknown formula id");
        let le = f1.as_int().unwrap().le(&f2.as_int().unwrap());
        let id = store_formula(Dynamic::from_ast(&le));
        SymExpr { id }
    }

    pub fn lt(self, other: SymExpr) -> Self {
        let f1 = get_formula(self.id).expect("unknown formula id");
        let f2 = get_formula(other.id).expect("unknown formula id");
        let lt = f1.as_int().unwrap().lt(&f2.as_int().unwrap());
        let id = store_formula(Dynamic::from_ast(&lt));
        SymExpr { id }
    }

    pub fn add(self, other: SymExpr) -> Self {
        let f1 = get_formula(self.id).expect("unknown formula id");
        let f2 = get_formula(other.id).expect("unknown formula id");
        let add = f1.as_int().unwrap().add(&f2.as_int().unwrap());
        let id = store_formula(Dynamic::from_ast(&add));
        SymExpr { id }
    }

    fn _add(self, other: Int) -> Self {
        let f1 = get_formula(self.id).expect("unknown formula id");
        let add = f1.as_int().unwrap().add(other);
        let id = store_formula(Dynamic::from_ast(&add));
        SymExpr { id }
    }

    pub fn sub(self, other: SymExpr) -> Self {
        let f1 = get_formula(self.id).expect("unknown formula id");
        let f2 = get_formula(other.id).expect("unknown formula id");
        let sub = f1.as_int().unwrap().sub(&f2.as_int().unwrap());
        let id = store_formula(Dynamic::from_ast(&sub));
        SymExpr { id }
    }

    fn _sub(self, other: Int) -> Self {
        let f1 = get_formula(self.id).expect("unknown formula id");
        let sub = f1.as_int().unwrap().sub(other);
        let id = store_formula(Dynamic::from_ast(&sub));
        SymExpr { id }
    }

    pub fn mul(self, other: SymExpr) -> Self {
        let f1 = get_formula(self.id).expect("unknown formula id");
        let f2 = get_formula(other.id).expect("unknown formula id");
        let mul = f1.as_int().unwrap().mul(&f2.as_int().unwrap());
        let id = store_formula(Dynamic::from_ast(&mul));
        SymExpr { id }
    }

    fn _mul(self, other: Int) -> Self {
        let f1 = get_formula(self.id).expect("unknown formula id");
        let mul = f1.as_int().unwrap().mul(other);
        let id = store_formula(Dynamic::from_ast(&mul));
        SymExpr { id }
    }

    pub fn div(self, other: SymExpr) -> Self {
        let f1 = get_formula(self.id).expect("unknown formula id");
        let f2 = get_formula(other.id).expect("unknown formula id");
        let div = f1.as_int().unwrap().div(&f2.as_int().unwrap());
        let id = store_formula(Dynamic::from_ast(&div));
        SymExpr { id }
    }

    fn _div(self, other: Int) -> Self {
        let f1 = get_formula(self.id).expect("unknown formula id");
        let div = f1.as_int().unwrap().div(other);
        let id = store_formula(Dynamic::from_ast(&div));
        SymExpr { id }
    }

    pub fn rem(self, other: SymExpr) -> Self {
        let f1 = get_formula(self.id).expect("unknown formula id");
        let f2 = get_formula(other.id).expect("unknown formula id");
        let rem = f1.as_int().unwrap().rem(&f2.as_int().unwrap());
        let id = store_formula(Dynamic::from_ast(&rem));
        SymExpr { id }
    }

    fn _rem(self, other: Int) -> Self {
        let f1 = get_formula(self.id).expect("unknown formula id");
        let rem = f1.as_int().unwrap().rem(other);
        let id = store_formula(Dynamic::from_ast(&rem));
        SymExpr { id }
    }

    pub fn id(&self) -> usize {
        self.id
    }
}

impl Add for SymExpr {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        self.add(other)
    }
}

impl Add<i64> for SymExpr {
    type Output = Self;

    fn add(self, other: i64) -> Self {
        self._add(Int::from_i64(other))
    }
}

impl Sub for SymExpr {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        self.sub(other)
    }
}

impl Sub<i64> for SymExpr {
    type Output = Self;

    fn sub(self, other: i64) -> Self {
        self._sub(Int::from_i64(other))
    }
}

impl Mul for SymExpr {
    type Output = Self;

    fn mul(self, other: Self) -> Self {
        self.mul(other)
    }
}

impl Mul<i64> for SymExpr {
    type Output = Self;

    fn mul(self, other: i64) -> Self {
        self._mul(Int::from_i64(other))
    }
}

impl Div for SymExpr {
    type Output = Self;

    fn div(self, other: Self) -> Self {
        self.div(other)
    }
}

impl Div<i64> for SymExpr {
    type Output = Self;

    fn div(self, other: i64) -> Self {
        self._div(Int::from_i64(other))
    }
}

impl Rem for SymExpr {
    type Output = Self;

    fn rem(self, other: Self) -> Self {
        self.rem(other)
    }
}

impl Rem<i64> for SymExpr {
    type Output = Self;

    fn rem(self, other: i64) -> Self {
        return self._rem(Int::from_i64(other));
    }
}

/// Create a fresh symbolic boolean symbol and return its wrapper.
pub fn fresh_bool() -> SymExpr {
    let f = Bool::fresh_const("!b!".as_ref());
    SymExpr {
        id: store_formula(Dynamic::from_ast(&f)),
    }
}

/// Create a fresh symbolic int symbol and return its wrapper.
pub fn fresh_int() -> SymExpr {
    let f = Int::fresh_const("!i!".as_ref());
    SymExpr {
        id: store_formula(Dynamic::from_ast(&f)),
    }
}

pub fn int_val(val: i64) -> SymExpr {
    let f = Int::from_i64(val);
    SymExpr {
        id: store_formula(Dynamic::from_ast(&f)),
    }
}
