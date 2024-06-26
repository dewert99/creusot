extern crate creusot_contracts;
use creusot_contracts::prusti_prelude::*;

pub trait MkRef {
    #[open]
    #[logic]
    fn mk_ref(&self) -> &Self {
        self
    }
}

impl<T> MkRef for T {}

#[ensures({let x = x.mk_ref(); x.0 == result})]
pub fn mk_ref_bad<'a>(x: Box<(u32, u32)>) -> u32 {
    (*x).0
}
