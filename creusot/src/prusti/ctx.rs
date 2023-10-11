use crate::error::{CreusotResult, Error};

use super::{parsing::Home, region_set::*, util::RegionReplacer};
use crate::prusti::{
    typeck::normalize,
    types::{prepare_display, Ty},
    with_static::{FixingRegionReplacer, StaticNormalizerDefIds},
    zombie::ZombieDefIds,
};
use rustc_infer::infer::region_constraints::Constraint;
use rustc_lint::Lint;
use rustc_middle::{
    ty,
    ty::{BoundRegionKind, ParamEnv, Region, TyCtxt, TypeFoldable},
};
use rustc_span::{
    def_id::{DefId, LocalDefId, CRATE_DEF_ID},
    Span, Symbol,
};
use std::{
    fmt::{Debug, Formatter},
    iter,
    ops::Deref,
};

const CURR_STR: &str = "'curr";
const OLD_STR: &str = "'old";

const OLD_IDX: u8 = 0;
const STATIC_IDX: u8 = 1;
const CURR_IDX: u8 = 2;
const OLD_REG_SET: RegionSet = RegionSet::singleton(OLD_IDX as u32);
pub(super) const STATIC_REG_SET: RegionSet = RegionSet::singleton(STATIC_IDX as u32);
pub(super) const CURR_REG_SET: RegionSet = RegionSet::singleton(CURR_IDX as u32);

#[derive(Debug)]
enum FnType {
    Logic { valid_states: RegionSet },
    Program,
}

pub(crate) struct InternedInfo<'tcx> {
    pub tcx: TyCtxt<'tcx>,
    pub curr_sym: Symbol,
    pub(super) static_replacer_info: StaticNormalizerDefIds,
    pub zombie_info: ZombieDefIds,
}

pub(crate) struct BaseCtx<'a, 'tcx> {
    pub interned: &'a InternedInfo<'tcx>,
    base_regions: Vec<Region<'tcx>>,
    pub(super) owner_id: LocalDefId,
    param_env: ParamEnv<'tcx>,
    fn_type: FnType,
}

impl<'a, 'tcx> Deref for BaseCtx<'a, 'tcx> {
    type Target = InternedInfo<'tcx>;

    fn deref(&self) -> &Self::Target {
        &self.interned
    }
}

#[derive(Debug)]
pub(crate) struct Ctx<'a, 'tcx> {
    pub base: BaseCtx<'a, 'tcx>,
    pub(super) relation: RegionRelation,
}

impl<'a, 'tcx> Deref for Ctx<'a, 'tcx> {
    type Target = BaseCtx<'a, 'tcx>;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

/// Primarily intended for logic functions with home signatures where since the homes might not be bound
/// allows adding to base_regions on the fly and waits to build the relation until then end
#[derive(Debug)]
pub(crate) struct PreCtx<'a, 'tcx> {
    pub base: BaseCtx<'a, 'tcx>,
}

impl<'a, 'tcx> Deref for PreCtx<'a, 'tcx> {
    type Target = BaseCtx<'a, 'tcx>;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl<'a, 'tcx> Debug for BaseCtx<'a, 'tcx> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BaseCtx")
            .field("base_regions", &self.base_regions)
            .field("owner_id", &self.owner_id)
            .field("param_env", &self.param_env)
            .finish_non_exhaustive()
    }
}

pub(super) fn dummy_region(tcx: TyCtxt<'_>, sym: Symbol) -> Region<'_> {
    let def_id = CRATE_DEF_ID.to_def_id();
    Region::new_free(tcx, def_id, BoundRegionKind::BrNamed(def_id, sym))
}

fn try_index_of<T: Eq>(s: &[T], x: &T) -> Option<usize> {
    Some(s.iter().enumerate().find(|&(_, y)| x == y)?.0)
}

fn index_of<T: Eq + Debug>(s: &[T], x: &T) -> usize {
    try_index_of(s, x).expect(&format!("{s:?} did not contain {x:?}"))
}

fn ty_regions<'tcx>(ty: ty::Ty<'tcx>, tcx: TyCtxt<'tcx>) -> Vec<Region<'tcx>> {
    let mut v = Vec::new();
    ty.fold_with(&mut RegionReplacer {
        tcx,
        f: |r| {
            v.push(r);
            r
        },
    });
    v
}

impl<'tcx> InternedInfo<'tcx> {
    pub(crate) fn new(tcx: TyCtxt<'tcx>) -> Self {
        Self {
            tcx,
            curr_sym: Symbol::intern(CURR_STR),
            static_replacer_info: StaticNormalizerDefIds::new(tcx),
            zombie_info: ZombieDefIds::new(tcx),
        }
    }

    pub(crate) fn mk_region(&self, rs: RegionSet) -> Region<'tcx> {
        rs.into_region(self.tcx)
    }

    pub(crate) fn old_region(&self) -> Region<'tcx> {
        self.mk_region(OLD_REG_SET)
    }

    pub(crate) fn curr_region(&self) -> Region<'tcx> {
        self.mk_region(CURR_REG_SET)
    }

    pub(crate) fn static_region(&self) -> Region<'tcx> {
        self.mk_region(STATIC_REG_SET)
    }
}

impl<'a, 'tcx> BaseCtx<'a, 'tcx> {
    pub(crate) fn base_regions(&self) -> impl Iterator<Item = Region<'tcx>> + '_ {
        self.base_regions.iter().copied()
    }

    pub(super) fn param_env(&self) -> ParamEnv<'tcx> {
        // We want to ignore outlives constraints
        self.param_env
    }

    pub(crate) fn lint(&self, lint: &'static Lint, span: Span, msg: impl Into<String>) {
        let hir_id = self.tcx.local_def_id_to_hir_id(self.owner_id);
        self.tcx.struct_span_lint_hir(lint, hir_id, span, msg.into(), |x| x)
    }

    fn try_home_to_region(&self, s: Symbol) -> Option<Region<'tcx>> {
        if s == self.curr_sym {
            return Some(self.curr_region());
        }
        for (idx, reg) in self.base_regions.iter().enumerate() {
            if Some(s) == reg.get_name() {
                return Some(RegionSet::singleton(idx as u32).into_region(self.tcx));
            }
        }
        None
    }

    fn user_region_to_region(&self, r: Region<'tcx>) -> Option<Region<'tcx>> {
        self.try_home_to_region(r.get_name()?)
    }

    fn new(interned: &'a InternedInfo<'tcx>, owner_id: DefId) -> Self {
        let tcx = interned.tcx;
        let curr_region = dummy_region(tcx, interned.curr_sym);
        let old_region = dummy_region(tcx, Symbol::intern(OLD_STR));
        let base_regions = vec![old_region, tcx.lifetimes.re_static, curr_region];
        let base: ParamEnv = tcx.param_env(owner_id);
        let fixed = base.fold_with(&mut FixingRegionReplacer { ctx: interned, f: |r| r });
        let erased = tcx.erase_regions(fixed);
        BaseCtx {
            interned,
            base_regions,
            owner_id: owner_id.expect_local(),
            fn_type: FnType::Logic { valid_states: RegionSet::EMPTY },
            param_env: erased,
        }
    }

    pub(crate) fn fix_user_ty_regions<T: TypeFoldable<TyCtxt<'tcx>>>(&self, t: T) -> T {
        let t = t.fold_with(&mut FixingRegionReplacer {
            ctx: self.interned,
            f: |r| self.user_region_to_region(r).unwrap_or(self.tcx.lifetimes.re_erased),
        });
        normalize(self, t)
    }

    fn make_relation(&self, iter: impl Iterator<Item = (u32, u32)>) -> RegionRelation {
        let n = self.base_regions.len() as u32;
        // make 'static outlive everything
        let iter = iter.chain((0..n).into_iter().map(|n| (STATIC_IDX.into(), n)));
        RegionRelation::new(self.base_regions.len(), iter)
    }

    pub(super) fn region_index_to_name(&self, idx: u32) -> Symbol {
        self.base_regions[idx as usize].get_name().unwrap_or(Symbol::intern("'_"))
    }

    fn is_logic(&self) -> bool {
        matches!(self.fn_type, FnType::Logic { .. })
    }
}

impl<'a, 'tcx> PreCtx<'a, 'tcx> {
    pub(crate) fn new(interned: &'a InternedInfo<'tcx>, owner_id: DefId) -> Self {
        PreCtx { base: BaseCtx::new(interned, owner_id) }
    }

    pub(super) fn home_to_region(&mut self, s: Symbol) -> Region<'tcx> {
        if let Some(x) = self.try_home_to_region(s) {
            return x;
        }
        let idx = self.base_regions.len();
        self.base.base_regions.push(dummy_region(self.tcx, s));
        // homes that are not declared
        RegionSet::singleton(idx as u32).into_region(self.tcx)
    }

    pub(super) fn map_parsed_home(&mut self, home: Home) -> Home<Region<'tcx>> {
        Home { data: self.home_to_region(home.data) }
    }

    /// Fixes an external region by converting it into a singleton set
    pub(super) fn fix_region(&mut self, r: Region<'tcx>) -> Region<'tcx> {
        if r.get_name() == Some(self.curr_sym) {
            return self.curr_region();
        }
        let idx = match try_index_of(&self.base_regions, &r) {
            Some(idx) => idx,
            None => {
                let idx = self.base_regions.len();
                self.base.base_regions.push(r);
                idx
            }
        };
        RegionSet::singleton(idx as u32).into_region(self.tcx)
    }

    pub(super) fn fix_regions<T: TypeFoldable<TyCtxt<'tcx>>>(&mut self, t: T) -> T {
        let t = t
            .fold_with(&mut FixingRegionReplacer { ctx: self.interned, f: |r| self.fix_region(r) });
        normalize(&*self, t)
    }

    pub(super) fn finish_for_logic(
        self,
        iter: impl Iterator<Item = (Region<'tcx>, Ty<'tcx>)>,
    ) -> Ctx<'a, 'tcx> {
        let mut valid_states = RegionSet::singleton(CURR_IDX.into());
        let reg_to_idx = |r: Region| RegionSet::from(r).try_into_singleton().unwrap();
        let iter = iter.flat_map(|(state, ty)| {
            let state_idx = reg_to_idx(state);
            valid_states = valid_states.union(RegionSet::singleton(state_idx));
            ty_regions(ty.ty, self.tcx).into_iter().map(move |r| (reg_to_idx(r), reg_to_idx(state)))
        });
        let relation = self.base.make_relation(iter);
        let base = BaseCtx { fn_type: FnType::Logic { valid_states }, ..self.base };
        Ctx { base, relation }
    }
}

impl<'a, 'tcx> Ctx<'a, 'tcx> {
    pub(super) fn new_for_spec(
        interned: &'a InternedInfo<'tcx>,
        owner_id: DefId,
    ) -> CreusotResult<Self> {
        let tcx = interned.tcx;
        let mut res = BaseCtx::new(interned, owner_id);
        res.fn_type = FnType::Program;
        let (rs, constraints) = super::variance::constraints_of_fn(tcx, owner_id.expect_local());
        let mut cur_region = None;

        // Add all regions (if any of them are 'curr replace the curr_region instead
        let curr_sym = res.curr_sym;
        res.base_regions.extend(rs.filter_map(|x| {
            if x.get_name() == Some(curr_sym) {
                cur_region = Some(x);
                None
            } else {
                Some(x)
            }
        }));
        if let Some(cur_region) = cur_region {
            res.base_regions[usize::from(CURR_IDX)] = cur_region
        }

        let mut failed = false;
        let reg_to_idx = |r| index_of(&res.base_regions, &r) as u32;
        let mut assert_not_curr = |r: Region<'tcx>| {
            if r.get_name() == Some(res.curr_sym) {
                failed = true;
            };
            r
        };
        let constraints = constraints
            .map(|c| match c {
                Constraint::VarSubReg(_, r1) => (reg_to_idx(assert_not_curr(r1)), CURR_IDX as u32),
                Constraint::RegSubReg(r2, r1) => (reg_to_idx(assert_not_curr(r1)), reg_to_idx(r2)),
                _ => (CURR_IDX.into(), CURR_IDX.into()),
            })
            .chain(iter::once((CURR_IDX.into(), OLD_IDX.into())));
        let relation = res.make_relation(constraints);
        if failed {
            return Err(Error::new(
                tcx.def_ident_span(owner_id).unwrap(),
                format!("{CURR_STR} region must not be blocked"),
            ));
        }
        Ok(Ctx { base: res, relation })
    }

    pub(super) fn curr_home(&self) -> Home {
        self.curr_sym.into()
    }

    /// Fixes an external region by converting it into a singleton set
    pub(super) fn fix_region(&self, r: Region<'tcx>) -> Region<'tcx> {
        if r.is_erased() {
            return RegionSet::UNIVERSE.into_region(self.tcx);
        }
        let idx = index_of(&self.base_regions, &r);
        let res = RegionSet::singleton(idx as u32);
        if self.relation.idx_outlived_by(CURR_IDX.into(), res) || self.is_logic() {
            res.into_region(self.tcx)
        } else {
            self.curr_region()
        }
    }

    pub(super) fn fix_regions<T: TypeFoldable<TyCtxt<'tcx>>>(&self, t: T) -> T {
        let t = t
            .fold_with(&mut FixingRegionReplacer { ctx: self.interned, f: |r| self.fix_region(r) });
        normalize(&*self, t)
    }

    pub(crate) fn try_move_state(&self, state: Region<'tcx>, span: Span) -> CreusotResult<()> {
        if state == self.tcx.lifetimes.re_erased {
            return Err(Error::new(span, "at_expiry must be given an explicit region"));
        } else if state == self.static_region() {
            return Err(Error::new(span, "Cannot move to 'static since it never expires"));
        }

        let dstate = prepare_display(state, self);
        let state = RegionSet::from(state);
        if state.try_into_singleton().is_none() {
            return Err(Error::new(span, format!("Cannot move to state set {dstate}")));
        };
        match self.fn_type {
            FnType::Logic { valid_states } if !state.subset(valid_states) => {
                Err(Error::new(span, format!("Cannot move to the state set {dstate} since it might have been instantiated with multiple states")))
            }
            _ => Ok(()),
        }
    }
}

pub(crate) type CtxRef<'a, 'tcx> = &'a Ctx<'a, 'tcx>;
