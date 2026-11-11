//! Three roles, implicit coupling across them, in one process and under MPI.
//!
//! The coupled linear system
//!
//! ```text
//! a'  = -a  + K b0          (role "a", 1 rank)
//! b0' = -2 b0 + K a         (role "b", the interface owner: 2 ranks under
//! b1' = -2 b1 + K c          MPI, one component each; 1 thread locally)
//! c'  = -c  + K b1          (role "c", 1 rank)
//! ```
//!
//! is advanced with backward Euler over coupling windows. `b` owns the
//! interface unknown `[b0, b1]`; every window is iterated with IQN-ILS until
//! `b`'s output stops changing. The same input runs as three threads in one
//! process and as four MPI ranks, and the two must agree bit for bit and with
//! a monolithic backward-Euler solve.

#![cfg(feature = "mpi")]

use grass_multi::{
    CoupledRun, CoupledRunner, DistributedImplicit, OuterIteration, Relaxation, RoleContext,
    WindowAction,
};
use std::process::Command;

const CHILD: &str = "GRASS_MULTI_THREE_ROLES_CHILD";
const K: f64 = 1.5;
const DT: f64 = 0.2;
const WINDOWS: u64 = 25;
const A0: f64 = 1.0;
const C0: f64 = -0.5;
const CONFIG: &str = r#"
    [topology]
    mode = "auto"
    [[topology.role]]
    name = "a"
    ranks = 1
    [[topology.role]]
    name = "b"
    ranks = 2
    [[topology.role]]
    name = "c"
    ranks = 1
"#;

fn f64s(bytes: &[u8]) -> Vec<f64> {
    bytes
        .chunks_exact(8)
        .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
        .collect()
}
fn bytes(v: &[f64]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn iteration() -> OuterIteration {
    OuterIteration::new(
        vec![0.0; 2],
        Relaxation::IqnIls {
            omega0: 0.5,
            reuse: 2,
            filter: 1e-10,
        },
        1e-13,
        50,
    )
}

/// Roles `a` and `c`: one scalar each, coupled to one component of `b`.
fn edge(mut ctx: RoleContext, component: usize, decay: f64, u0: f64) -> Vec<f64> {
    let ex = ctx.exchange("b");
    let mut ic = DistributedImplicit::new(&ctx, "b", iteration(), DT, WINDOWS);
    let (mut u, mut saved) = (u0, u0);
    while !ic.finished() {
        match ic.begin() {
            WindowAction::Save => saved = u,
            WindowAction::Restore => u = saved,
        }
        // b's current guess, as b's shards in rank order
        let guess: Vec<f64> = ex
            .exchange(&[])
            .unwrap()
            .iter()
            .flat_map(|s| f64s(s))
            .collect();
        u = (u + DT * K * guess[component]) / (1.0 + decay * DT);
        ex.exchange(&bytes(&[u])).unwrap();
        ic.end(None);
    }
    assert!(ic.unconverged_windows().is_empty());
    let mut out = vec![u];
    out.extend(ic.iterations_per_window().iter().map(|i| *i as f64));
    out
}

/// Role `b`: owns the interface `[b0, b1]`, split over its ranks.
fn middle(mut ctx: RoleContext) -> Vec<f64> {
    let to_a = ctx.exchange("a");
    let to_c = ctx.exchange("c");
    let (rank, size) = ctx.role_collective().position();
    let mine: Vec<usize> = if size == 1 {
        vec![0, 1]
    } else {
        vec![rank as usize]
    };
    let mut ic = DistributedImplicit::new(&ctx, "b", iteration(), DT, WINDOWS);
    let mut u: Vec<f64> = vec![0.0; mine.len()];
    let mut saved = u.clone();
    let mut guess = u.clone();
    while !ic.finished() {
        match ic.begin() {
            WindowAction::Save => saved = u.clone(),
            WindowAction::Restore => u = saved.clone(),
        }
        to_a.exchange(&bytes(&guess)).unwrap();
        to_c.exchange(&bytes(&guess)).unwrap();
        let a = f64s(&to_a.exchange(&[]).unwrap()[0])[0];
        let c = f64s(&to_c.exchange(&[]).unwrap()[0])[0];
        for (k, comp) in mine.iter().enumerate() {
            let drive = if *comp == 0 { a } else { c };
            u[k] = (saved[k] + DT * K * drive) / (1.0 + 2.0 * DT);
        }
        let (_, next) = ic.end(Some(&u));
        guess = next.unwrap();
    }
    u
}

/// Monolithic backward Euler on [a, b0, b1, c].
fn reference() -> [f64; 4] {
    let mut x = [A0, 0.0, 0.0, C0];
    for _ in 0..WINDOWS {
        let old = x;
        // Gauss-Seidel to machine precision on the 4x4 system.
        for _ in 0..500 {
            x[0] = (old[0] + DT * K * x[1]) / (1.0 + DT);
            x[1] = (old[1] + DT * K * x[0]) / (1.0 + 2.0 * DT);
            x[2] = (old[2] + DT * K * x[3]) / (1.0 + 2.0 * DT);
            x[3] = (old[3] + DT * K * x[2]) / (1.0 + DT);
        }
    }
    x
}

fn child() {
    let run = CoupledRunner::from_source(CONFIG)
        .unwrap()
        .role("a", |ctx| edge(ctx, 0, 1.0, A0))
        .role("b", middle)
        .role("c", |ctx| edge(ctx, 1, 1.0, C0))
        .run()
        .expect("run three roles");
    let rank = grass_mpi_rank();
    let lines: Vec<(String, Vec<f64>)> = match run {
        CoupledRun::Local(all) => all,
        CoupledRun::Split { role, result } => vec![(format!("{role}{rank}"), result)],
    };
    for (role, v) in lines {
        let bits: Vec<String> = v.iter().map(|x| format!("{:016x}", x.to_bits())).collect();
        println!("RESULT {role} {}", bits.join(","));
    }
}

fn grass_mpi_rank() -> String {
    std::env::var("OMPI_COMM_WORLD_RANK").unwrap_or_default()
}

fn parse(out: &str) -> Vec<(String, Vec<f64>)> {
    let mut v: Vec<(String, Vec<f64>)> = out
        .lines()
        .filter_map(|l| l.split_once("RESULT ").map(|(_, rest)| rest))
        .map(|l| {
            let (role, vals) = l.split_once(' ').unwrap();
            let vals = vals
                .split(',')
                .map(|h| f64::from_bits(u64::from_str_radix(h, 16).unwrap()))
                .collect();
            (role.to_string(), vals)
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

#[test]
fn three_roles_implicit_local_and_split_agree() {
    if std::env::var_os(CHILD).is_some() {
        child();
        return;
    }
    if Command::new("mpirun").arg("--version").output().is_err() {
        eprintln!("SKIP three-role coupling: `mpirun` not found");
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let args = [
        "--exact",
        "three_roles_implicit_local_and_split_agree",
        "--nocapture",
        "--test-threads=1",
    ];
    let local = Command::new(&exe)
        .args(args)
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        local.status.success(),
        "{}",
        String::from_utf8_lossy(&local.stderr)
    );
    let split = Command::new("mpirun")
        .args(["--oversubscribe", "-np", "4"])
        .arg(&exe)
        .args(args)
        .env(CHILD, "1")
        .env("OMPI_MCA_btl", "self,vader")
        .output()
        .unwrap();
    assert!(
        split.status.success(),
        "{}",
        String::from_utf8_lossy(&split.stderr)
    );

    let local = parse(&String::from_utf8_lossy(&local.stdout));
    let split = parse(&String::from_utf8_lossy(&split.stdout));
    let get = |v: &[(String, Vec<f64>)], k: &str| {
        v.iter()
            .find(|(r, _)| r == k)
            .unwrap_or_else(|| panic!("no result for `{k}` in {v:?}"))
            .1
            .clone()
    };

    // local: a, b (both components), c; split: a0, b1, b2, c3 (world ranks)
    let (la, lb, lc) = (get(&local, "a"), get(&local, "b"), get(&local, "c"));
    let (sa, sb0, sb1, sc) = (
        get(&split, "a0"),
        get(&split, "b1"),
        get(&split, "b2"),
        get(&split, "c3"),
    );
    assert_eq!(
        la, sa,
        "role a: local and split differ (incl. iteration counts)"
    );
    assert_eq!(lc, sc);
    assert_eq!(lb, vec![sb0[0], sb1[0]], "b's components bit-identical");

    let r = reference();
    let got = [la[0], lb[0], lb[1], lc[0]];
    for (g, r) in got.iter().zip(r) {
        assert!(
            (g - r).abs() < 1e-11,
            "partitioned {got:?} vs monolithic {r:?}"
        );
    }
    let iters = &la[1..];
    assert_eq!(iters.len(), WINDOWS as usize);
    assert!(
        iters.iter().all(|i| *i <= 6.0),
        "IQN iterations per window {iters:?}"
    );
}
