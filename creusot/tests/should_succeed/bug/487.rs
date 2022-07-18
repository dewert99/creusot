extern crate creusot_contracts;
use creusot_contracts::*;

pub fn test() {
    let c = {
        #[requires(@x + @y < @u32::MAX)]
        #[ensures(@result == @x + @y)]
        |x: u32, y: u32| x + y
    };
    let z = c(2, 3);
    proof_assert!(@z == 5)
}