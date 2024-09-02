use crate::{logic::Mapping, *};

#[cfg_attr(creusot, creusot::builtins = "prelude.set.Set.set")]
pub struct Set<T: ?Sized>(std::marker::PhantomData<T>);

impl<T: ?Sized> Set<T> {
    #[cfg(creusot)]
    #[trusted]
    #[creusot::builtins = "prelude.set.Set.empty"]
    pub const EMPTY: Self = { Set(std::marker::PhantomData) };

    #[doc(hidden)]
    #[logic]
    #[open(self)]
    #[creusot::builtins = "prelude.prelude.Cast.id"]
    pub fn from_map(_: Mapping<T, bool>) -> Self {
        absurd
    }

    #[doc(hidden)]
    #[logic]
    #[open(self)]
    #[creusot::builtins = "prelude.prelude.Cast.id"]
    pub fn to_map(self) -> Mapping<T, bool> {
        absurd
    }

    #[open]
    #[predicate]
    #[why3::attr = "inline:trivial"]
    pub fn contains(self, e: T) -> bool {
        self.to_map().get(e)
    }

    #[open]
    #[logic]
    #[why3::attr = "inline:trivial"]
    pub fn insert(self, e: T) -> Self {
        Self::from_map(self.to_map().set(e, true))
    }

    #[open(self)]
    #[predicate]
    #[why3::attr = "inline:trivial"]
    pub fn is_empty(self) -> bool {
        self == Self::EMPTY
    }

    #[open]
    #[logic]
    #[why3::attr = "inline:trivial"]
    pub fn remove(self, a: T) -> Self {
        Self::from_map(self.to_map().set(a, false))
    }

    #[open]
    #[logic]
    #[why3::attr = "inline:trivial"]
    pub fn union(self, other: Self) -> Self
    where
        T: Sized,
    {
        Self::from_map(|x| self.contains(x) || other.contains(x))
    }
}
