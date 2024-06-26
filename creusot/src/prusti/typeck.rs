use super::{
    flat_ty, full_signature_logic,
    parsing::{parse_home_sig_lit, HomeSig},
    region_set::StateSet,
    typeck::MutDerefType::{Cur, Fin},
    types::*,
    util::RegionReplacer,
    FnSigBinder,
};
use crate::{
    error::{CreusotResult, Error},
    lints::PRUSTI_ZOMBIE,
    prusti::{
        ctx::{BaseCtx, CtxRef, InternedInfo, STATIC_STATE},
        flat_rust_ty::{flatten_rust_ty, walk_with_rust_flat_ty, RustReg},
        flat_ty::{flat_to_ty, flatten_ty, CheckSupError, FlatTy},
        parsing::Outlives,
        region_set::State,
        zombie::{pretty_replace, ZombieStatus},
    },
    util,
};
use itertools::Either;
use rustc_ast::Mutability;
use rustc_data_structures::sso::SsoHashMap;
use rustc_hir::LangItem;
use rustc_index::{bit_set::BitSet, IndexVec};
use rustc_infer::{
    infer::{InferCtxt, TyCtxtInferExt},
    traits::{Obligation, ObligationCause},
};
use rustc_middle::{
    bug, span_bug, ty,
    ty::{
        AdtDef, Binder, BoundRegionKind, ClauseKind, GenericArg, GenericArgs, GenericArgsRef,
        GenericParamDefKind, GenericPredicates, InferTy, Instance, InstantiatedPredicates,
        PolyFnSig, PredicateKind, Region, RegionKind, RegionVid, TyCtxt, TyKind, TyVid,
        TypeFoldable, TypeFolder, TypeSuperFoldable, TypeSuperVisitable, TypeVisitable,
        TypeVisitor,
    },
};
use rustc_span::{def_id::DefId, Span, Symbol, DUMMY_SP};
use rustc_target::abi::VariantIdx;
use rustc_trait_selection::{
    traits,
    traits::{query::evaluate_obligation::InferCtxtExt, NormalizeExt, ObligationCtxt},
};
use std::{fmt::Debug, iter, ops::ControlFlow};

type SmallVec<T> = smallvec::SmallVec<[T; 4]>;

pub fn def_id_to_span(tcx: TyCtxt<'_>, id: DefId) -> Span {
    tcx.def_ident_span(id).unwrap_or(DUMMY_SP)
}

fn bound_reg_to_span(tcx: TyCtxt<'_>, r: BoundRegionKind) -> Span {
    match r {
        BoundRegionKind::BrAnon => DUMMY_SP,
        BoundRegionKind::BrNamed(x, _) => def_id_to_span(tcx, x),
        BoundRegionKind::BrEnv => DUMMY_SP,
    }
}

pub fn reg_to_span<'tcx>(tcx: TyCtxt<'tcx>, r: Region<'tcx>) -> Span {
    match r.kind() {
        RegionKind::ReEarlyParam(x) => def_id_to_span(tcx, x.def_id),
        RegionKind::ReBound(_, x) => bound_reg_to_span(tcx, x.kind),
        RegionKind::ReLateParam(x) => bound_reg_to_span(tcx, x.bound_region),
        RegionKind::RePlaceholder(x) => bound_reg_to_span(tcx, x.bound.kind),
        _ => DUMMY_SP,
    }
}

fn home_sig(ctx: CtxRef<'_, '_>, def_id: DefId) -> CreusotResult<Option<HomeSig>> {
    let home_sig = util::get_attr_lit(ctx.tcx, def_id, &["creusot", "prusti", "home_sig"]);
    match home_sig {
        Some(home_sig) => Ok(parse_home_sig_lit(home_sig)?),
        None => Ok(None),
    }
}

trait Captures<'tcx> {}
impl<'tcx, X> Captures<'tcx> for X {}

fn filter_elided<'tcx>(
    iter: impl Iterator<Item = (Region<'tcx>, Region<'tcx>)>,
) -> impl Iterator<Item = (Symbol, RegionVid)> + Captures<'tcx> {
    let el_name = Symbol::intern("'_");
    iter.into_iter().filter_map(move |(k, v): (Region, Region)| {
        assert!(v.is_var());
        match k.get_name() {
            Some(name) if name != el_name => Some((name, v.as_var())),
            _ => None, // Ignore elided regions
        }
    })
}

/// Bound on a region variable
#[derive(Debug, Eq, PartialEq, Copy, Clone)]
enum ReVarStatus {
    /// Must be a superset
    Bound(StateSet),
    /// Must match exactly
    Exact(StateSet),
}

#[derive(Debug)]
struct TyVarInfo<'tcx>(IndexVec<u32, (FlatTy, Ty<'tcx>)>);

impl<'tcx> TyVarInfo<'tcx> {
    fn add_bound(&mut self, key: u32, ty: Ty<'tcx>, ctx: CtxRef<'_, 'tcx>) {
        flat_ty::union(ctx, &mut self.0[key].0, ty)
    }
    fn get_ty(&self, key: u32, ctx: CtxRef<'_, 'tcx>) -> Ty<'tcx> {
        let (flat, skeleton) = &self.0[key];
        flat_to_ty(ctx, flat, *skeleton)
    }
}

/// Maps region variables to there bounds
#[derive(Debug)]
struct RegVarInfo(IndexVec<RegionVid, ReVarStatus>);

impl RegVarInfo {
    fn force_exact(&mut self, idx: RegionVid, exact: StateSet) -> Result<(), CheckSupError> {
        match self.0[idx] {
            ReVarStatus::Bound(expected) if !expected.subset(exact) => {
                return Err(CheckSupError::StateMismatch { expected, found: exact })
            }
            ReVarStatus::Exact(expected) if expected != exact => {
                return Err(CheckSupError::StateMismatch { expected, found: exact })
            }
            _ => {}
        }
        self.0[idx] = ReVarStatus::Exact(exact);
        Ok(())
    }

    fn add_bound(&mut self, expected: RustReg, found: StateSet) -> Result<(), CheckSupError> {
        match expected {
            RustReg::Static => {
                let expected = StateSet::singleton(STATIC_STATE);
                if found.subset(expected) {
                    Ok(())
                } else {
                    Err(CheckSupError::StateMismatch { expected, found })
                }
            }
            RustReg::Var(v) => match self.0[v] {
                ReVarStatus::Bound(ss) => {
                    self.0[v] = ReVarStatus::Bound(ss.union(found));
                    Ok(())
                }
                ReVarStatus::Exact(expected) => {
                    if found.subset(expected) {
                        Ok(())
                    } else {
                        Err(CheckSupError::StateMismatch { expected, found })
                    }
                }
            },
        }
    }

    fn get_region<'tcx>(&self, key: Region<'tcx>, ctx: CtxRef<'_, 'tcx>) -> Region<'tcx> {
        let ss = match key.kind() {
            RegionKind::ReVar(vid) => match self.0[vid] {
                ReVarStatus::Bound(ss) => ss,
                ReVarStatus::Exact(ss) => ss,
            },
            RegionKind::ReStatic => StateSet::singleton(STATIC_STATE),
            _ => bug!(),
        };
        ctx.mk_region(ss)
    }

    fn get_exact_stateset(&self, vid: RegionVid) -> Option<StateSet> {
        match self.0[vid] {
            ReVarStatus::Bound(_) => None,
            ReVarStatus::Exact(ss) => Some(ss),
        }
    }
}

#[derive(Debug)]
struct VarInfo<'tcx> {
    reg: RegVarInfo,
    ty: TyVarInfo<'tcx>,
    subst: GenericArgsRef<'tcx>,
    def_id: DefId,
}

struct VarFolder<'a, 'tcx>(&'a VarInfo<'tcx>, CtxRef<'a, 'tcx>);

impl<'a, 'tcx> TypeFolder<TyCtxt<'tcx>> for VarFolder<'a, 'tcx> {
    fn interner(&self) -> TyCtxt<'tcx> {
        self.1.tcx
    }

    fn fold_ty(&mut self, t: ty::Ty<'tcx>) -> ty::Ty<'tcx> {
        match t.kind() {
            TyKind::Infer(InferTy::FreshTy(n)) => self.0.ty.get_ty(*n, self.1).ty,
            _ => t.super_fold_with(self),
        }
    }

    fn fold_region(&mut self, r: Region<'tcx>) -> Region<'tcx> {
        self.0.reg.get_region(r, self.1)
    }
}

impl<'tcx> VarInfo<'tcx> {
    fn fold<T: TypeFoldable<TyCtxt<'tcx>>>(&self, t: T, ctx: CtxRef<'_, 'tcx>) -> T {
        t.fold_with(&mut VarFolder(self, ctx))
    }
    fn fold_ty(
        &self,
        ty: ty::Ty<'tcx>,
        ctx: CtxRef<'_, 'tcx>,
        span: Span,
    ) -> CreusotResult<Ty<'tcx>> {
        let final_subst = self.fold(self.subst, ctx);
        let predicates: GenericPredicates<'tcx> = ctx.tcx.predicates_of(self.def_id);
        check_predicates(ctx, predicates.instantiate(ctx.tcx, final_subst), span)?;
        Ok(Ty { ty: self.fold(ty, ctx) })
    }
}

/// Expand error when expected is a local type
fn expand_error_sup<'tcx>(
    ctx: CtxRef<'_, 'tcx>,
    span: Span,
    expected: Ty<'tcx>,
    found: Ty<'tcx>,
    err: CheckSupError,
) -> Error {
    let expected_raw = display_fold(expected.ty, ctx);
    let found_d = prepare_display(found, ctx);
    match err {
        CheckSupError::ZombieMismatch => {
            Error::new(span, format!("expected `{expected_raw}` found `{found_d}`"))
        }
        CheckSupError::StateMismatch { expected, found } => {
            let expected_r = make_region_for_display(expected, &ctx.base);
            let found_r = make_region_for_display(found, &ctx.base);
            let msg = format!("function was supposed to return data with type `{expected_raw}` but it is returning data with type `{found_d}`\n\
                            expected `{expected_r}` found `{found_r}`");
            Error::new(span, msg)
        }
    }
}

/// Expand error when expected is a generalize rust type
fn expand_error_gen<'tcx>(
    ctx: CtxRef<'_, 'tcx>,
    span: Span,
    var_info: &VarInfo<'tcx>,
    ty_gen: ty::Ty<'tcx>,
    ty: Ty<'tcx>,
    err: CheckSupError,
) -> Error {
    let tcx = ctx.tcx;
    let replacer = |r: Region<'tcx>| match r.kind() {
        RegionKind::ReVar(vid) => match var_info.reg.get_exact_stateset(vid) {
            None => tcx.lifetimes.re_erased,
            Some(ss) => make_region_for_display(ss, ctx),
        },
        RegionKind::ReStatic => tcx.lifetimes.re_static,
        _ => tcx.lifetimes.re_erased,
    };
    let ty_gen = pretty_replace(ctx.interned, replacer, ty_gen);
    let ty = prepare_display(ty, ctx);
    match err {
        CheckSupError::ZombieMismatch => {
            Error::new(span, format!("expected `{ty_gen}` found `{ty}`"))
        }
        CheckSupError::StateMismatch { expected, found } => {
            let expected_r = make_region_for_display(expected, &ctx.base);
            let found_r = make_region_for_display(found, &ctx.base);
            let msg = format!("the expression's lifetime `{found_r}` must match the current time slice `{expected_r}` (found `{ty}`, expected `{ty_gen}`)");
            Error::new(span, msg)
        }
    }
}

pub(super) struct TypeVarVisitor<'a>(pub &'a mut BitSet<u32>);

impl<'a, 'tcx> TypeVisitor<TyCtxt<'tcx>> for TypeVarVisitor<'a> {
    type Result = ();
    fn visit_ty(&mut self, ty: ty::Ty<'tcx>) {
        match ty.kind() {
            TyKind::Param(p) => {
                self.0.insert(p.index);
            }
            _ => ty.super_visit_with(self),
        }
    }
}

struct AliasTyVarVisitor(BitSet<u32>);

impl<'tcx> TypeVisitor<TyCtxt<'tcx>> for AliasTyVarVisitor {
    type Result = ();
    fn visit_ty(&mut self, ty: ty::Ty<'tcx>) {
        match ty.kind() {
            TyKind::Alias(_, ty) => ty.args.visit_with(&mut TypeVarVisitor(&mut self.0)),
            _ => ty.super_visit_with(self),
        }
    }
}

fn generalize<'tcx, I: Iterator<Item = ty::Ty<'tcx>>>(
    ctx: CtxRef<'_, 'tcx>,
    subst: GenericArgsRef<'tcx>,
    def_id: DefId,
    check_tys: impl Fn(GenericArgsRef<'tcx>) -> I,
) -> VarInfo<'tcx> {
    let tcx = ctx.tcx;
    let mut reg = RegVarInfo(IndexVec::new());
    let mut ty = TyVarInfo(IndexVec::new());
    let mut constrained_vars = AliasTyVarVisitor(BitSet::new_empty(subst.len()));
    for check_ty in check_tys(GenericArgs::identity_for_item(tcx, def_id)) {
        let _ = check_ty.visit_with(&mut constrained_vars);
    }
    let res = GenericArgs::for_item(tcx, def_id, |param, _| {
        let elt: GenericArg<'tcx> = subst[param.index as usize];
        match param.kind {
            GenericParamDefKind::Type { .. } if !constrained_vars.0.contains(param.index) => {
                let elt = elt.as_type().unwrap();
                let elt = ctx.fix_ty(elt, || ctx.interned.mk_region(StateSet::EMPTY));
                let flat = flatten_ty(ctx, elt);
                ty::Ty::new_fresh(tcx, ty.0.push((flat, elt))).into()
            }
            _ => elt.fold_with(&mut RegionReplacer {
                tcx,
                f: |_| Region::new_var(tcx, reg.0.push(ReVarStatus::Bound(StateSet::EMPTY))),
            }),
        }
    });
    VarInfo { reg, ty, subst: res, def_id }
}

fn check_gen<'tcx>(
    ctx: CtxRef<'_, 'tcx>,
    span: Span,
    ty: Ty<'tcx>,
    ty_gen: ty::Ty<'tcx>,
    var_info: &mut VarInfo<'tcx>,
) -> CreusotResult<()> {
    let flat_gen = flatten_rust_ty(ty_gen);
    let res = walk_with_rust_flat_ty(
        ctx,
        &flat_gen,
        ty,
        |found, expected| var_info.reg.add_bound(expected, found),
        |found, expected| Ok(var_info.ty.add_bound(expected, found, ctx)),
    );
    res.map_err(|err| expand_error_gen(ctx, span, var_info, ty_gen, ty, err))
}

fn generalize_fn_def<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: DefId,
    var_info: &mut VarInfo<'tcx>,
) -> (ty::Ty<'tcx>, impl Iterator<Item = (Region<'tcx>, Region<'tcx>)>) {
    let fn_ty_gen = ty::Ty::new_fn_def(tcx, def_id, var_info.subst);
    let (fn_sig_gen, map) = tcx.instantiate_bound_regions(fn_ty_gen.fn_sig(tcx), |_| {
        Region::new_var(tcx, var_info.reg.0.push(ReVarStatus::Bound(StateSet::EMPTY)))
    });
    let fn_ty_gen = ty::Ty::new_fn_ptr(tcx, Binder::dummy(fn_sig_gen));

    let id_subst = GenericArgs::identity_for_item(tcx, def_id);
    let iter1 = id_subst.regions().zip(var_info.subst.regions());
    let iter2 = map.into_iter().map(move |(br, reg_gen)| {
        let reg = Region::new_late_param(tcx, def_id, br.kind);
        (reg, reg_gen)
    });
    let iter = iter1.chain(iter2);
    (fn_ty_gen, iter)
}

pub(crate) fn check_call<'tcx>(
    ctx: CtxRef<'_, 'tcx>,
    state: State,
    def_id: DefId,
    subst_ref: GenericArgsRef<'tcx>,
    args: impl Iterator<Item = CreusotResult<((State, Ty<'tcx>), Span)>>,
    call_span: Span,
) -> CreusotResult<Ty<'tcx>> {
    let tcx = ctx.tcx;
    // Eagerly evaluate args to avoid running multiple inference contexts at the same time
    let mut args = args.collect::<CreusotResult<SmallVec<_>>>()?;
    let home_sig = home_sig(ctx, def_id)?;
    let (home_sig_args, home_sig_bounds) = match &home_sig {
        Some(home_sig) => (Either::Left(home_sig.args()), Either::Left(home_sig.bounds())),
        None => (Either::Right(iter::repeat(ctx.now_sym)), Either::Right(iter::empty())),
    };

    let mut var_info = generalize(ctx, subst_ref, def_id, |s| {
        let x = ty::Ty::new_fn_def(tcx, def_id, s).fn_sig(tcx).skip_binder();
        x.inputs().iter().copied().chain([x.output()])
    });
    let (fn_ty_gen, iter) = generalize_fn_def(tcx, def_id, &mut var_info);
    let fn_ty_gen = normalize(ctx, fn_ty_gen);
    let subst_iter = filter_elided(iter);

    let (args_gen, res_ty_gen) = match fn_ty_gen.kind() {
        TyKind::FnPtr(bind) => {
            let bind: &PolyFnSig = bind;
            let sig = bind.no_bound_vars().unwrap();
            (sig.inputs(), sig.output())
        }
        _ => span_bug!(call_span, "bug"),
    };

    // maps homes in the home signature to states that were passed in for them
    let mut constrained_homes = SsoHashMap::default();

    for (arg, home_sig_arg) in args.iter_mut().zip(home_sig_args) {
        let ((from_state, ty), span) = arg;
        if home_sig_arg == ctx.now_sym {
            *ty = check_move_state(*from_state, state, ctx, *ty, *span)?;
        } else {
            match constrained_homes.insert(home_sig_arg, *from_state) {
                Some(oth_state) if oth_state != *from_state => {
                    let d_oth_ts = display_state(oth_state, ctx);
                    let d_this_ts = display_state(*from_state, ctx);
                    return Err(Error::new(*span, format!("expected argument to come from state `{d_oth_ts}`, but it came from `{d_this_ts}`\n\
                    required by home signature of function")));
                }
                _ => {}
            }
        }
    }
    constrained_homes.insert(ctx.now_sym, state); // 'curr is always constrained to the current state

    // check explicit constraints
    for Outlives { long, short } in home_sig_bounds {
        if let (Some(&long), Some(&short)) =
            (constrained_homes.get(&long), constrained_homes.get(&short))
        {
            if !ctx.relation.outlives_state(StateSet::singleton(long), short) {
                let dlong = display_state(long, ctx);
                let dshort = display_state(short, ctx);
                let msg = format!(
                    "expected `{dlong}` to outlive `{dshort}`\n\
                    required by home signature of function"
                );
                return Err(Error::new(call_span, msg));
            }
        }
    }

    for (home, var) in subst_iter {
        if let Some(&constraint) = constrained_homes.get(&home) {
            var_info.reg.force_exact(var, StateSet::singleton(constraint)).unwrap();
        }
    }

    for (((_, ty), span), ty_gen) in args.into_iter().zip(args_gen) {
        check_gen(ctx, span, ty, *ty_gen, &mut var_info)?;
    }

    let res = var_info.fold_ty(res_ty_gen, ctx, call_span)?;
    if let Some(r) = ty_outlives(res, state, ctx) {
        let dty = prepare_display(res, ctx);
        let dstate = display_state(state, ctx);
        let r = prepare_display(r, ctx);
        let msg = format!("`{dty}` cannot be returned in `{dstate}` since it doesn't live long enough\n`{r}` doesn't outlive `{dstate}`");
        return Err(Error::new(call_span, msg));
    }
    Ok(res)
}

pub(crate) fn check_constructor<'tcx>(
    ctx: CtxRef<'_, 'tcx>,
    fields: impl Iterator<Item = CreusotResult<(Ty<'tcx>, Span)>>,
    subst: GenericArgsRef<'tcx>,
    adt: AdtDef<'tcx>,
    variant: VariantIdx,
    span: Span,
) -> CreusotResult<Ty<'tcx>> {
    let tcx = ctx.tcx;
    // Eagerly evaluate args to avoid running multiple inference contexts at the same time
    let fields = fields.collect::<CreusotResult<SmallVec<_>>>()?.into_iter();
    let mut var_info = generalize(ctx, subst, adt.did(), |s| {
        adt.variant(variant).fields.iter().map(|x| x.ty(tcx, s))
    });
    let fields_gen =
        adt.variant(variant).fields.iter().map(|x| normalize(ctx, x.ty(tcx, var_info.subst)));

    fields
        .zip(fields_gen)
        .try_for_each(|((ty, span), ty_gen)| check_gen(ctx, span, ty, ty_gen, &mut var_info))?;
    var_info.fold_ty(ty::Ty::new_adt(tcx, adt, var_info.subst), ctx, span)
}

pub(crate) fn check_tuple_constructor<'tcx>(
    ctx: CtxRef<'_, 'tcx>,
    fields: impl Iterator<Item = CreusotResult<(Ty<'tcx>, Span)>>,
) -> CreusotResult<Ty<'tcx>> {
    let tcx = ctx.tcx;
    let fields = fields.map(|x| Ok(x?.0.ty)).collect::<CreusotResult<SmallVec<_>>>()?;
    Ok(Ty { ty: ty::Ty::new_tup(tcx, &fields) }.pack(ZombieStatus::NonZombie, ctx))
}

pub(crate) fn union<'tcx>(
    ctx: CtxRef<'_, 'tcx>,
    target: ty::Ty<'tcx>,
    elts: impl Iterator<Item = CreusotResult<(Ty<'tcx>, Span)>>,
) -> CreusotResult<Ty<'tcx>> {
    let mut elts = elts.map(|elt| Ok(elt?.0)).collect::<CreusotResult<SmallVec<_>>>()?.into_iter();
    match elts.next() {
        None => Ok(ctx.fix_ty(target, || ctx.interned.mk_region(StateSet::EMPTY))),
        Some(ty) => match elts.next() {
            None => Ok(ty),
            Some(ty2) => {
                let mut flat = flatten_ty(ctx, ty);
                flat_ty::union(ctx, &mut flat, ty2);
                for ty2 in elts {
                    flat_ty::union(ctx, &mut flat, ty2)
                }
                Ok(flat_to_ty(ctx, &flat, ty))
            }
        },
    }
}

pub(super) fn normalize<'tcx, T: Debug + TypeFoldable<TyCtxt<'tcx>>>(
    ctx: &'_ BaseCtx<'_, 'tcx>,
    ty: T,
) -> T {
    let tcx = ctx.tcx;
    let infcx = tcx.infer_ctxt().build();
    infcx.at(&ObligationCause::dummy(), ctx.param_env()).normalize(ty).value
}

pub(crate) fn check_sup<'tcx>(
    ctx: CtxRef<'_, 'tcx>,
    expected: Ty<'tcx>,
    actual: Ty<'tcx>,
    span: Span,
) -> CreusotResult<()> {
    let expected_flat = flatten_ty(ctx, expected);
    flat_ty::check_sup(ctx, expected_flat, actual)
        .map_err(|err| expand_error_sup(ctx, span, expected, actual, err))
}

pub(crate) fn try_resolve<'tcx>(
    ctx: CtxRef<'_, 'tcx>,
    def_id: DefId,
    args: GenericArgsRef<'tcx>,
) -> (DefId, GenericArgsRef<'tcx>) {
    match Instance::resolve(ctx.tcx, ctx.param_env(), def_id, args) {
        Err(_) | Ok(None) => (def_id, args), // Can't specialize
        Ok(Some(inst)) => (inst.def.def_id(), inst.args),
    }
}

struct AllRegionsOutliveCheck<'a, 'tcx> {
    ctx: CtxRef<'a, 'tcx>,
    state: State,
}

impl<'a, 'tcx> TypeVisitor<TyCtxt<'tcx>> for AllRegionsOutliveCheck<'a, 'tcx> {
    type Result = ControlFlow<Region<'tcx>>;

    fn visit_region(&mut self, r: Region<'tcx>) -> Self::Result {
        if self.ctx.relation.outlives_state(r.into(), self.state) {
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break(r)
        }
    }
}

fn ty_outlives<'tcx>(ty: Ty<'tcx>, state: State, ctx: CtxRef<'_, 'tcx>) -> Option<Region<'tcx>> {
    ty.ty.visit_with(&mut AllRegionsOutliveCheck { ctx, state }).break_value()
}

fn is_plain<'tcx>(ctx: CtxRef<'_, 'tcx>, ty: Ty<'tcx>) -> bool {
    let trait_def_id = ctx.plain_def_id;
    let infcx = ctx.tcx.infer_ctxt().ignoring_regions().build();
    let param_env = ctx.param_env();
    traits::type_known_to_meet_bound_modulo_regions(&infcx, param_env, ty.ty, trait_def_id)
}

fn check_predicates<'tcx>(
    ctx: CtxRef<'_, 'tcx>,
    predicates: InstantiatedPredicates<'tcx>,
    span: Span,
) -> CreusotResult<()> {
    let infcx = ctx.tcx.infer_ctxt().ignoring_regions().build();
    for (pred, _) in ctx.tcx.erase_regions(predicates) {
        let ok = match pred.as_trait_clause() {
            Some(x) if x.def_id() == ctx.zombie_info.snap_eq() => {
                let ty = ctx.tcx.normalize_erasing_late_bound_regions(ctx.param_env(), x.self_ty());
                Ty { ty }.is_snap_eq(ctx)
            }
            _ => {
                let ob = Obligation::new(
                    ctx.tcx,
                    ObligationCause::dummy_with_span(span),
                    ctx.param_env(),
                    pred.as_predicate(),
                );
                infcx.evaluate_obligation_no_overflow(&ob).must_apply_modulo_regions()
            }
        };
        if !ok {
            return Err(Error::new(span, format!("Unsatisfied bound {}", pred)));
        }
    }
    Ok(())
}

pub(crate) fn check_move_state<'tcx>(
    from_state: State,
    to_state: State,
    ctx: CtxRef<'_, 'tcx>,
    ty: Ty<'tcx>,
    span: Span,
) -> CreusotResult<Ty<'tcx>> {
    let dty = prepare_display(ty, ctx);
    let d_to_ts = display_state(to_state, ctx);
    let d_from_ts = display_state(from_state, ctx);
    if to_state == from_state {
        Ok(ty)
    } else if let Some(r) = ty_outlives(ty, to_state, ctx) {
        let r = prepare_display(r, ctx);
        Err(Error::new(span, format!("`{dty}` cannot be moved from `{d_from_ts}` to `{d_to_ts}` since it doesn't live long enough\n`{r}` doesn't outlive `{d_to_ts}`")))
    } else if !(ctx.relation.outlives_state(StateSet::singleton(to_state), from_state)
        || is_plain(ctx, ty))
    {
        Err(Error::new(
            span,
            format!("`{dty}` cannot be moved from `{d_from_ts}` to `{d_to_ts}` since it didn't exist at that point"),
        ))
    } else {
        let (rty, is_zombie) = ty.mk_zombie(ctx);
        if is_zombie && !ty.ty.is_mutable_ptr() {
            let rty = prepare_display(rty, ctx);
            let msg = format!("`{dty}` cannot be moved from `{d_from_ts}` to `{d_to_ts}` without becoming a zombie `{rty}`");
            ctx.lint(PRUSTI_ZOMBIE, span, msg)
        }
        Ok(rty)
    }
}

pub(crate) enum MutDerefType {
    Cur,
    Fin,
}

pub(crate) fn mut_deref<'tcx>(
    state: State,
    ctx: CtxRef<'_, 'tcx>,
    ty: Ty<'tcx>,
    span: Span,
) -> CreusotResult<(MutDerefType, Ty<'tcx>)> {
    match ty.as_ref(ctx) {
        Some((end, nty, zombie, Mutability::Mut)) => {
            match (StateSet::from(end) == StateSet::singleton(state), zombie) {
                (true, ZombieStatus::Zombie) => Ok((Fin, nty)),
                (false, ZombieStatus::NonZombie) => Ok((Cur, nty)),
                (true, ZombieStatus::NonZombie) => {
                    ctx.lint(crate::lints::PRUSTI_AMBIGUITY, span, "ambiguous dereference");
                    Ok((Cur, nty))
                }
                (false, ZombieStatus::Zombie) => {
                    let end = prepare_display(end, ctx);
                    let state = display_state(state, ctx);
                    Err(Error::new(span, format!("invalid mut dereference of zombie expression with lifetime `{end}` in state `{state}`")))
                }
            }
        }
        Some((lft, _, _, Mutability::Not)) => {
            let ty = shr_deref(state, ctx, ty, span)?.0;
            let (op, rty) = mut_deref(state, ctx, ty, span)?;
            Ok((op, Ty::make_ref(lft, rty, ctx)))
        }
        _ => span_bug!(span, "bug"),
    }
}

pub(crate) fn shr_deref<'tcx>(
    state: State,
    ctx: CtxRef<'_, 'tcx>,
    ty: Ty<'tcx>,
    span: Span,
) -> CreusotResult<(Ty<'tcx>, Region<'tcx>)> {
    let Some((end, nty, _, Mutability::Not)) = ty.as_ref(ctx) else { span_bug!(span, "bug") };
    // if ts has it's home in the current state we should know it's lifetime is longer than it's home
    if ctx.relation.outlives_state(end.into(), state) {
        Ok((nty, end))
    } else {
        let end = prepare_display(end, ctx);
        let ts = display_state(state, ctx);
        span_bug!(span, "invalid shr reference with lifetime `{end}` existed in state `{ts}`");
    }
}

pub(crate) fn box_deref<'tcx>(
    _state: State,
    ctx: CtxRef<'_, 'tcx>,
    ty: Ty<'tcx>,
    span: Span,
) -> CreusotResult<Ty<'tcx>> {
    match ty.unpack(ctx) {
        (ZombieStatus::NonZombie, ty) => match ty.kind() {
            &TyKind::Adt(adt, subst) if adt.is_box() => {
                Ok(Ty { ty: subst.types().next().unwrap() })
            }
            _ => span_bug!(span, "bug"),
        },
        (ZombieStatus::Zombie, _) => {
            Err(Error::new(span, "invalid box dereference of zombie expression".to_string()))
        }
    }
}

pub(crate) fn mk_ref<'tcx>(
    _state: State,
    lft: Region<'tcx>,
    ctx: CtxRef<'_, 'tcx>,
    ty: Ty<'tcx>,
    _span: Span,
) -> CreusotResult<Ty<'tcx>> {
    Ok(Ty::make_ref(lft, ty, ctx))
}

pub(crate) fn check_signature_agreement<'tcx>(
    tcx: TyCtxt<'tcx>,
    impl_id: DefId,
    trait_id: DefId,
    refn_subst: GenericArgsRef<'tcx>,
) -> CreusotResult<()> {
    use rustc_ast::{token, MetaItemLit as Lit};
    let trait_home_sig = util::get_attr_lit(tcx, trait_id, &["creusot", "prusti", "home_sig"]);
    let Some(trait_home_sig) = trait_home_sig else {
        return Ok(()); // We're not specializing a home signature
    };
    let impl_id_subst = GenericArgs::identity_for_item(tcx, impl_id);
    let impl_span: Span = tcx.def_span(impl_id);
    let ts = Lit::from_token_lit(
        token::Lit { kind: token::Str, symbol: Symbol::intern("curr"), suffix: None },
        impl_span,
    );
    let ts = ts.ok().unwrap();

    let interned = InternedInfo::new(tcx);
    let (ctx, ts, arg_tys, (_, expect_res_ty)) = full_signature_logic::<SmallVec<_>>(
        &interned,
        trait_home_sig,
        FnSigBinder::for_trait_impl(tcx, trait_id, impl_id, refn_subst),
        &ts,
    )?;
    let args = arg_tys.into_iter().map(|(_, ty)| Ok((ty, impl_span)));
    // lifetimes bound from the impl block that aren't used in the Self type are excluded
    // we can erase these lifetimes since they will disappear after substitution
    let subst_ref = ctx.fix_subst_with_erased(impl_id_subst);
    let actual_res_ty = check_call(&ctx, ts, impl_id, subst_ref, args, impl_span)?;
    debug!(
        "{impl_id:?}: expected {}, found {}",
        prepare_display(expect_res_ty, &ctx),
        prepare_display(actual_res_ty, &ctx)
    );
    check_sup(&ctx, expect_res_ty, actual_res_ty, impl_span)
}

/// Makes a type copy by wrapping parts of it in Zombie
/// result.1 is true iff the type was changed
pub(super) fn mk_zombie<'tcx>(ty: ty::Ty<'tcx>, ctx: CtxRef<'_, 'tcx>) -> (ty::Ty<'tcx>, bool) {
    let tcx = ctx.tcx;
    let param_env = ctx.param_env();
    if ty.is_copy_modulo_regions(tcx, param_env) {
        (ty, false)
    } else {
        let copy_id = tcx.require_lang_item(LangItem::Copy, None);
        let sized_id = tcx.require_lang_item(LangItem::Sized, None);
        let mut infcx = ctx.tcx.infer_ctxt().ignoring_regions().build();
        let ty_gen = ty.super_fold_with(&mut ZombieGenFolder(&mut infcx));
        let mut need_copy = BitSet::new_empty(infcx.num_ty_vars());
        let ocx = ObligationCtxt::new(&infcx);
        ocx.register_bound(ObligationCause::dummy(), param_env, ty_gen, copy_id);
        for ob in ocx.select_all_or_error() {
            match ob.obligation.predicate.kind().skip_binder() {
                PredicateKind::Clause(ClauseKind::Trait(x))
                    if x.def_id() == copy_id && x.self_ty().is_ty_var() =>
                {
                    // Mark that this variable needs to be copy by unifying it with ()
                    ocx.eq(&ObligationCause::dummy(), param_env, tcx.types.unit, x.self_ty())
                        .unwrap();
                }
                PredicateKind::Clause(ClauseKind::Trait(x))
                    if x.def_id() == sized_id && x.self_ty().is_ty_var() =>
                {
                    // Ignore _: Sized bounds since T: Sized => Zombie<T>: Sized
                }
                _ => {
                    // If there are any other obligations required to make ty: Copy we need to wrap the whole thing
                    // in a zombie
                    let ty = ctx.zombie_info.mk_zombie_raw(ty, tcx);
                    return (ty, true);
                }
            }
        }
        for i in 0..need_copy.domain_size() {
            let vid = TyVid::from(i);
            if !infcx.resolve_vars_if_possible(ty::Ty::new_var(tcx, vid)).is_ty_var() {
                // Store all marked type variables in set
                // (This needs to be done in two steps since the FulfillmentErrors may contain
                // equivalent but not identical type variables)
                need_copy.insert(vid);
            }
        }
        (ty.super_fold_with(&mut ZombieFolder(ctx, 0, need_copy)), true)
    }
}

struct ZombieFolder<'a, 'tcx>(CtxRef<'a, 'tcx>, usize, BitSet<TyVid>);

impl<'a, 'tcx> TypeFolder<TyCtxt<'tcx>> for ZombieFolder<'a, 'tcx> {
    fn interner(&self) -> TyCtxt<'tcx> {
        self.0.tcx
    }

    fn fold_ty(&mut self, ty: ty::Ty<'tcx>) -> ty::Ty<'tcx> {
        let res = if self.2.contains(self.1.into()) { mk_zombie(ty, self.0).0 } else { ty };
        self.1 += 1;
        res
    }
}

struct ZombieGenFolder<'a, 'tcx>(&'a mut InferCtxt<'tcx>);

impl<'a, 'tcx> TypeFolder<TyCtxt<'tcx>> for ZombieGenFolder<'a, 'tcx> {
    fn interner(&self) -> TyCtxt<'tcx> {
        self.0.tcx
    }

    fn fold_ty(&mut self, _: ty::Ty<'tcx>) -> ty::Ty<'tcx> {
        self.0.next_ty_var(DUMMY_SP)
    }

    fn fold_region(&mut self, _: Region<'tcx>) -> Region<'tcx> {
        self.0.tcx.lifetimes.re_erased
    }
}
