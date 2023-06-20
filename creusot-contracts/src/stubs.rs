use crate as creusot_contracts;
use creusot_contracts::{logic, open};

#[creusot::no_translate]
#[rustc_diagnostic_item = "fin"]
pub fn fin<T: ?Sized>(_: &mut T) -> Box<T> {
    panic!()
}

#[creusot::no_translate]
#[rustc_diagnostic_item = "cur"]
pub fn cur<T>(_: &mut T) -> T {
    panic!()
}

#[creusot::no_translate]
#[rustc_diagnostic_item = "equal"]
pub fn equal<T: ?Sized>(_: T, _: T) -> bool {
    panic!();
}

#[creusot::no_translate]
#[rustc_diagnostic_item = "neq"]
pub fn neq<T: ?Sized>(_: T, _: T) -> bool {
    panic!();
}

#[creusot::no_translate]
#[rustc_diagnostic_item = "exists"]
pub fn exists<T, F: Fn(T) -> bool>(_: F) -> bool {
    panic!()
}

#[creusot::no_translate]
#[rustc_diagnostic_item = "forall"]
pub fn forall<T, F: Fn(T) -> bool>(_: F) -> bool {
    panic!()
}

#[creusot::no_translate]
#[rustc_diagnostic_item = "implication"]
pub fn implication(_: bool, _: bool) -> bool {
    panic!();
}

#[creusot::no_translate]
#[rustc_diagnostic_item = "old"]
pub fn old<T>(_: T) -> T {
    panic!()
}

#[creusot::no_translate]
#[rustc_diagnostic_item = "absurd"]
pub fn abs<T>() -> T {
    panic!()
}

#[creusot::no_translate]
#[rustc_diagnostic_item = "variant_check"]
pub fn variant_check<R: crate::well_founded::WellFounded>(r: R) -> R {
    r
}

#[creusot::no_translate]
#[rustc_diagnostic_item = "closure_result_constraint"]
pub fn closure_result<R>(_: R, _: R) {}

#[logic] // avoid triggering error since this is prusti specific
#[open]
#[creusot::no_translate]
#[rustc_diagnostic_item = "prusti_curr"]
pub fn curr<T>(_: T) -> T {
    absurd
}

#[logic] // avoid triggering error since this is prusti specific
#[open]
#[creusot::no_translate]
#[rustc_diagnostic_item = "prusti_expiry"]
pub fn at_expiry<'a: 'a, T>(_: T) -> T {
    absurd
}

#[logic] // avoid triggering error since this is prusti specific
#[open]
#[creusot::no_translate]
#[rustc_diagnostic_item = "prusti_dbg_ty"]
pub fn __dbg_ty<T>(_: T) -> T {
    absurd
}
