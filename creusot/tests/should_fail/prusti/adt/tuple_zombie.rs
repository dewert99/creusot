extern crate creusot_contracts;
use creusot_contracts::prusti_prelude::*;

#[open]
#[logic('x, 'now where 'now: 'x)]
pub fn test_constructor<'x>(x: Box<u32>, y: Box<u32>) -> (Box<u32>, Box<u32>) {
    (at::<'x>(x), y)
}
