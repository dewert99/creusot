extern crate creusot_contracts;
use creusot_contracts::prusti_prelude::*;

pub trait Trait {}

impl<'a> Trait for &'a mut u32 {}
#[logic]
fn id<X: Trait>(x: X) -> X {
    x
}

#[ensures(*id(x) == 5u32)]
fn test_id(x: &mut u32) {
    *x = 5
}
