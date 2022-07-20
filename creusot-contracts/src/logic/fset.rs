use crate as creusot_contracts;
use creusot_contracts_proc::*;

use crate::{logic::*, Int};
use std::ops::Index;

#[creusot::builtins = "set.Fset.fset"]
pub struct FSet<T: ?Sized>(std::marker::PhantomData<T>);

impl<T: ?Sized> FSet<T> {
    #[trusted]
    #[creusot::builtins = "set.Fset.empty"]
    pub const EMPTY: Self = { FSet(std::marker::PhantomData) };

    #[predicate]
    #[why3::attr = "inline:trivial"]
    pub fn contains(self, e: T) -> bool {
        Self::mem(e, self)
    }

    #[creusot::builtins = "set.Fset.mem"]
    fn mem(e: T, set: Self) -> bool {
        pearlite! { absurd }
    }

    #[logic]
    #[why3::attr = "inline:trivial"]
    pub fn insert(self, e: T) -> Self {
        Self::add(e, self)
    }

    #[logic]
    #[creusot::builtins = "set.Fset.add"]
    fn add(e: T, set: Self) -> Self {
        pearlite! { absurd }
    }

    #[predicate]
    #[creusot::builtins = "set.Fset.is_empty"]
    pub fn is_empty(self) -> bool {
        pearlite! { absurd }
    }

    #[logic]
    #[why3::attr = "inline:trivial"]
    pub fn remove(self, a: T) -> Self {
        Self::rem(a, self)
    }

    #[logic]
    #[creusot::builtins = "set.Fset.remove"]
    pub fn rem(a: T, s: Self) -> Self {
        pearlite! { absurd}
    }

    #[logic]
    #[creusot::builtins = "set.Fset.union"]
    pub fn union(self, a: Self) -> Self {
        pearlite! { absurd}
    }

    #[logic]
    #[creusot::builtins = "set.Fset.cardinal"]
    pub fn len(self) -> Int {
        pearlite! { absurd }
    }
}
