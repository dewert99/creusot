use crate::{
    error::{CreusotResult, Error},
    prusti::{
        ctx::{Ctx, CtxRef, InternedInfo, PreCtx},
        fn_sig_binder::FnSigBinder,
        parsing,
        parsing::{HomeSig, Outlives},
        region_set::State,
        types::Ty,
    },
    util,
};
use itertools::{Either, Itertools};
use rustc_ast::MetaItemLit as Lit;
use rustc_middle::{bug, ty::FnSig};
use rustc_span::{def_id::DefId, symbol::Ident, Span, Symbol, DUMMY_SP};
use std::iter;

/// Returns region corresponding to `l`
/// Also checks that 'post is not blocked
fn make_state<'tcx>(l: &Lit, ctx: CtxRef<'_, 'tcx>) -> CreusotResult<State> {
    let pre_region = ctx.pre_state(DUMMY_SP).unwrap();
    let post_region = ctx.post_state(DUMMY_SP).unwrap();
    let mut regions =
        ctx.base_states().iter_enumerated().map(|(s, &r)| (r.get_name(), ctx.normalize_state(s)));
    let sym = l.as_token_lit().symbol;
    match sym.as_str() {
        "old" => Ok(pre_region),
        "curr" => Ok(post_region),
        "'_" => {
            let mut candiates =
                (&mut regions).filter(|(r, fix)| r.is_none() && *fix != post_region);
            match (candiates.next(), candiates.next()) {
                (None, _) => Err(Error::new(l.span, "function has no blocked anonymous regions")),
                (Some(_), Some(_)) => {
                    Err(Error::new(l.span, "function has multiple blocked anonymous regions"))
                }
                (Some((_, r)), None) => Ok(r),
            }
        }
        _ => {
            if let Some((_, r)) = regions.find(|(r, _)| *r == Some(sym)) {
                Ok(r)
            } else if sym == ctx.now_sym {
                ctx.now_state(l.span) // this should fail but we want its error message
            } else {
                Err(Error::new(l.span, format!("use of undeclared lifetime name {sym}")))
            }
        }
    }
}

/// Returns region corresponding to `l` in a logical context
fn make_state_logic<'tcx>(l: &Lit, ctx: &mut PreCtx<'_, 'tcx>) -> CreusotResult<State> {
    let now_state = ctx.now_state(DUMMY_SP).unwrap();
    let sym = l.as_token_lit().symbol;
    match sym.as_str() {
        "old" => Ok(now_state), //hack requires clauses to use same time slice as return
        "curr" => Ok(now_state),
        "'_" => Err(Error::new(
            l.span,
            "expiry contract on logic function must use a concrete lifetime/home",
        )),
        _ => Ok(ctx.home_to_state(sym, l.span)?),
    }
}

type BindingInfo<'tcx> = (State, Ty<'tcx>);
type Binding<'tcx> = (Symbol, BindingInfo<'tcx>);

fn add_homes_to_sig<'a, 'tcx, T: FromIterator<Binding<'tcx>>>(
    args: &'tcx [Ident],
    sig: FnSig<'tcx>,
    arg_homes: impl Iterator<Item = CreusotResult<State>>,
    ret_home: State,
    _span: Span,
) -> CreusotResult<(T, BindingInfo<'tcx>)> {
    let types = sig.inputs().iter().zip(arg_homes);

    let arg_tys = args
        .iter()
        .enumerate()
        .map(|(idx, arg)| {
            if arg.name.is_empty() {
                let name = format!("{}", util::AnonymousParamName(idx));
                Symbol::intern(&name)
            } else {
                arg.name
            }
        })
        .zip(types);
    let arg_tys = arg_tys
        .map(|(k, (&ty, home))| Ok((k, (home?, Ty { ty }))))
        .collect::<CreusotResult<T>>()?;
    let res_ty = Ty { ty: sig.output() };
    Ok((arg_tys, (ret_home, res_ty)))
}

pub(crate) fn full_signature<'a, 'tcx, T: FromIterator<Binding<'tcx>>>(
    interned: &'a InternedInfo<'tcx>,
    ts: &Lit,
    owner_id: DefId,
) -> CreusotResult<(Ctx<'a, 'tcx>, State, T, BindingInfo<'tcx>)> {
    let tcx = interned.tcx;
    let sig = FnSigBinder::new(tcx, owner_id);
    let ctx = Ctx::new_for_spec(interned, sig)?;
    let sig = tcx.liberate_late_bound_regions(owner_id, sig.sig());
    let sig = ctx.fix_regions(sig, || bug!());
    let span = ts.span;

    let ts = make_state(ts, &ctx)?;
    let pre_state = ctx.pre_state(span)?;
    let arg_homes = iter::repeat_with(|| Ok(pre_state));
    let ret_home = ctx.post_state(span)?;
    let args = ctx.tcx.fn_arg_names(ctx.owner_id);
    let (arg_tys, res_ty) = add_homes_to_sig(args, sig, arg_homes, ret_home, DUMMY_SP)?;
    Ok((ctx, ts, arg_tys, res_ty))
}

fn validate_home_sig(home_sig: &HomeSig, ctx: &PreCtx, span: Span) -> CreusotResult<()> {
    for bound in home_sig.bounds().flat_map(|Outlives { long, short }| [long, short]) {
        if bound != ctx.now_sym && !home_sig.args().contains(&bound) {
            let msg = format!(
                "signature uses the state `{bound}` in a constraint but not for any argument"
            );
            return Err(Error::new(span, msg));
        }
    }
    Ok(())
}

pub(crate) fn full_signature_logic<'a, 'tcx, T: FromIterator<Binding<'tcx>>>(
    interned: &'a InternedInfo<'tcx>,
    home_sig_lit: &Lit,
    sig: FnSigBinder<'tcx>,
    ts: &Lit,
) -> CreusotResult<(Ctx<'a, 'tcx>, State, T, BindingInfo<'tcx>)> {
    let tcx = interned.tcx;
    let mut ctx = PreCtx::new(interned, sig)?;
    let sig = tcx.liberate_late_bound_regions(sig.def_id().to_def_id(), sig.sig());
    let sig = ctx.fix_regions(sig);

    let ts2 = make_state_logic(ts, &mut ctx)?;
    let args = ctx.tcx.fn_arg_names(ctx.owner_id);
    let now_state = ctx.now_state(DUMMY_SP).unwrap();
    let hs_span = home_sig_lit.span;
    let home_sig = parsing::parse_home_sig_lit(home_sig_lit)?;
    let (arg_homes, bounds) = match &home_sig {
        Some(home_sig) if home_sig.args_len() != sig.inputs().len() => {
            return Err(Error::new(hs_span, "number of args doesn't match signature"));
        }
        Some(home_sig) => {
            validate_home_sig(home_sig, &ctx, hs_span)?;
            let arg_homes = home_sig.args().map(|h| ctx.home_to_state(h, hs_span));
            (Either::Left(arg_homes), Either::Left(home_sig.bounds()))
        }
        None => (Either::Right(iter::repeat_with(|| Ok(now_state))), Either::Right(iter::empty())),
    };

    let (arg_tys, res_ty) = add_homes_to_sig::<Vec<_>>(args, sig, arg_homes, now_state, hs_span)?;
    let iter = IntoIterator::into_iter(&arg_tys).map(|(_, x)| *x);
    let iter = iter.chain(iter::once(res_ty));

    let ctx = ctx.finish_for_logic(iter, bounds);
    ctx.try_move_state(ts2, ts.span)?;
    Ok((ctx, ts2, arg_tys.into_iter().collect(), res_ty))
}
