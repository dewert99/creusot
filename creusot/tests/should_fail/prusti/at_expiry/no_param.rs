extern crate creusot_contracts;
use creusot_contracts::prusti_prelude::*;

#[after_expiry('a, at(true))]
fn test<'a>(x: &'a mut u32) -> &mut u32 {
    x
}
