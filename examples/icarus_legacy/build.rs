//! Builds the legacy Fortran (minus its `program`), the C-interface shim and
//! the mock aeroheating code into one static library, and links the MPI
//! Fortran runtime. The stand-alone program keeps its own Makefile.
use std::{env, path::PathBuf, process::Command};

const LEGACY: &[&str] = &[
    "mi_globals",
    "mi_parallel",
    "mi_input",
    "mi_mesh",
    "mi_solver",
    "mi_physics",
    "mi_output",
    "mi_driver",
];

fn run(cmd: &mut Command) -> String {
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("failed to run {cmd:?}: {e}"));
    assert!(
        out.status.success(),
        "{cmd:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let fc = env::var("MPIFC").unwrap_or_else(|_| "mpif90".into());
    let cc = env::var("CC").unwrap_or_else(|_| "cc".into());

    let mut objs = Vec::new();
    let sources = LEGACY
        .iter()
        .map(|m| format!("fortran/{m}.f90"))
        .chain(["adapter/micarus_capi.f90".to_string()]);
    for src in sources {
        let stem = PathBuf::from(&src).file_stem().unwrap().to_owned();
        let obj = out.join(stem).with_extension("o");
        run(Command::new(&fc)
            .args(["-O2", "-g", "-fPIC", "-c", &src, "-J"])
            .arg(&out)
            .arg("-o")
            .arg(&obj));
        objs.push(obj);
        println!("cargo:rerun-if-changed={src}");
    }
    let aero = out.join("aero.o");
    run(Command::new(&cc)
        .args(["-O2", "-fPIC", "-c", "../mock_icarus/c/aero.c", "-o"])
        .arg(&aero));
    objs.push(aero);
    println!("cargo:rerun-if-changed=../mock_icarus/c/aero.c");
    let mut ar = Command::new("ar");
    ar.arg("crs").arg(out.join("liblegacy.a")).args(&objs);
    run(&mut ar);
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=legacy");

    // The MPI Fortran bindings and runtime, as the wrapper compiler reports
    // them (OpenMPI: --showme:link, MPICH: -link_info).
    let link = Command::new(&fc)
        .arg("--showme:link")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_else(|| run(Command::new(&fc).arg("-link_info")));
    for tok in link.split_whitespace() {
        if let Some(dir) = tok.strip_prefix("-L") {
            println!("cargo:rustc-link-search=native={dir}");
        } else if let Some(lib) = tok.strip_prefix("-l") {
            println!("cargo:rustc-link-lib=dylib={lib}");
        }
    }
    println!("cargo:rustc-link-lib=dylib=gfortran");
}
