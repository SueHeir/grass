//! One role of the matrix, run by `CoupledRunner` as a thread or on MPI ranks.

use std::sync::Arc;

use grass_multi::{
    CouplingEpoch, DistributedImplicit, EntityId, OuterIteration, RoleContext, RoleExchange,
    RoutedPayload, RoutedRoleExchange, WindowAction,
};

use crate::model::{Case, System};

pub struct Output {
    pub values: Vec<(usize, f64)>,
    pub iterations: Vec<u32>,
    pub report: bool,
    pub unconverged: usize,
}

enum Link {
    Bridge(Box<dyn RoleExchange>),
    Routed(RoutedRoleExchange, u64),
}

fn to_bytes(v: &[f64]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn from_bytes(b: &[u8]) -> Vec<f64> {
    b.chunks_exact(8)
        .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

impl Link {
    /// Send this rank's `values` for components `mine` (or nothing), and put
    /// whatever the peer role sent into `ext`.
    fn swap(
        &mut self,
        sys: &System,
        peer: usize,
        mine: std::ops::Range<usize>,
        values: Option<&[f64]>,
        ext: &mut [f64],
    ) {
        match self {
            Link::Bridge(ex) => {
                let out = values.map(to_bytes).unwrap_or_default();
                let shards = ex.exchange(&out).expect("bridge exchange");
                let got: Vec<f64> = shards.iter().flat_map(|s| from_bytes(s)).collect();
                if !got.is_empty() {
                    let range = sys.role_range(peer);
                    assert_eq!(got.len(), range.len(), "peer sent a partial role vector");
                    ext[range].copy_from_slice(&got);
                }
            }
            Link::Routed(ex, epoch) => {
                let peer_size = ex.peer_size() as usize;
                let mut out = Vec::new();
                if let Some(values) = values {
                    for (k, i) in mine.clone().enumerate() {
                        for rho in 0..peer_size {
                            let needs = sys
                                .block(peer, rho, peer_size)
                                .any(|p| sys.inter[p].iter().any(|(j, _)| *j == i));
                            if needs {
                                out.push(RoutedPayload::new(
                                    rho as i32,
                                    EntityId(i as u64),
                                    values[k].to_le_bytes().to_vec(),
                                ));
                            }
                        }
                    }
                }
                let got = ex
                    .exchange(CouplingEpoch(*epoch), &out)
                    .expect("routed exchange");
                *epoch += 1;
                for rec in got {
                    let j = rec.entity_id.0 as usize;
                    assert_eq!(sys.role_of[j], peer, "record from the wrong role");
                    ext[j] = f64::from_le_bytes(rec.payload[..8].try_into().unwrap());
                }
            }
        }
    }
}

pub fn run_role(mut ctx: RoleContext, case: Case, sys: Arc<System>) -> Output {
    let r: usize = ctx.role()[1..].parse().unwrap();
    let rc = ctx.role_collective();
    let (rank, size) = rc.position();
    let mine = sys.block(r, rank as usize, size as usize);
    let neighbors = sys.neighbors(r);
    let mut links: Vec<(usize, Link)> = neighbors
        .iter()
        .map(|&q| {
            let name = Case::role_name(q);
            let link = match case.exchange.as_str() {
                "bridge" => Link::Bridge(ctx.exchange(&name)),
                "routed" => Link::Routed(ctx.routed_exchange(&name), 0),
                e => panic!("exchange `{e}`"),
            };
            (q, link)
        })
        .collect();

    let gs = case.gauss_seidel();
    let owners: Vec<usize> = if gs {
        sys.gs_owners()
    } else {
        (0..sys.roles()).collect()
    };
    let is_owner = owners.contains(&r);
    let mut ic = case.implicit().then(|| {
        let x0: Vec<f64> = owners
            .iter()
            .flat_map(|&q| sys.role_range(q).map(|i| sys.u0[i]))
            .collect();
        let names: Vec<String> = owners.iter().map(|&q| Case::role_name(q)).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        DistributedImplicit::with_owners(
            &ctx,
            &names,
            OuterIteration::new(x0, case.relaxation(), 1e-12, 60),
            case.dt,
            case.windows,
        )
    });

    let mut u: Vec<f64> = mine.clone().map(|i| sys.u0[i]).collect();
    let mut guess = u.clone();
    let mut saved = u.clone();
    let mut window = 0u64;
    loop {
        match &mut ic {
            Some(ic) => {
                if ic.finished() {
                    break;
                }
                match ic.begin() {
                    WindowAction::Save => saved = u.clone(),
                    WindowAction::Restore => u = saved.clone(),
                }
                window = ic.window();
            }
            None => {
                if window == case.windows {
                    break;
                }
            }
        }
        if case.fail_role.as_deref() == Some(ctx.role()) && window == case.fail_window && rank == 0
        {
            panic!("injected failure in role {} at window {window}", ctx.role());
        }

        // The whole role's window-start state, for the intra-role terms.
        let gathered = rc.gather_f64(&u);
        let full = rc.scatter_f64(gathered.map(|p| vec![p.concat(); size as usize]));
        let mut start = vec![f64::NAN; sys.n];
        start[sys.role_range(r)].copy_from_slice(&full);

        // What the neighbours see before anyone steps this sweep.
        let sweep_value: &[f64] = if case.implicit() && is_owner {
            &guess
        } else {
            &u
        };
        let mut ext = vec![f64::NAN; sys.n];
        let new = if gs {
            for (q, link) in links.iter_mut() {
                let send = (*q < r).then_some(sweep_value);
                link.swap(&sys, *q, mine.clone(), send, &mut ext);
            }
            for (q, link) in links.iter_mut().filter(|(q, _)| *q < r) {
                link.swap(&sys, *q, mine.clone(), None, &mut ext);
            }
            let new = sys.step(mine.clone(), &start, &ext, case.dt, case.substeps(r));
            for (q, link) in links.iter_mut().filter(|(q, _)| *q > r) {
                link.swap(&sys, *q, mine.clone(), Some(&new), &mut ext);
            }
            new
        } else {
            for (q, link) in links.iter_mut() {
                link.swap(&sys, *q, mine.clone(), Some(sweep_value), &mut ext);
            }
            sys.step(mine.clone(), &start, &ext, case.dt, case.substeps(r))
        };
        u = new;
        match &mut ic {
            Some(ic) => {
                let (_, next) = ic.end(is_owner.then_some(&u[..]));
                if let Some(next) = next {
                    guess = next;
                }
            }
            None => window += 1,
        }
    }
    let unconverged = ic.as_ref().map_or(0, |ic| ic.unconverged_windows().len());
    Output {
        unconverged,
        values: mine.zip(u).collect(),
        iterations: ic
            .map(|ic| ic.iterations_per_window().to_vec())
            .unwrap_or_default(),
        report: r == 0 && rank == 0,
    }
}
