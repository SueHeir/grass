//! Compiles the Fortran ablator and the C aeroheating code into one static
//! library with the system compilers. Nothing here is GRASS-specific: it is
//! what any Rust crate does to link a legacy library.
use std::{env, path::PathBuf, process::Command};

fn run(cmd: &mut Command) {
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("failed to run {cmd:?}: {e}"));
    assert!(status.success(), "{cmd:?} failed");
}

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let fc = env::var("FC").unwrap_or_else(|_| "gfortran".into());
    let cc = env::var("CC").unwrap_or_else(|_| "cc".into());

    let abl = out.join("ablator.o");
    let aero = out.join("aero.o");
    run(Command::new(&fc)
        .args(["-O2", "-fPIC", "-c", "fortran/ablator.f90", "-J"])
        .arg(&out)
        .arg("-o")
        .arg(&abl));
    run(Command::new(&cc)
        .args(["-O2", "-fPIC", "-c", "c/aero.c", "-o"])
        .arg(&aero));
    run(Command::new("ar")
        .arg("crs")
        .arg(out.join("libmockcodes.a"))
        .arg(&abl)
        .arg(&aero));

    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=mockcodes");
    println!("cargo:rustc-link-lib=dylib=gfortran");
    println!("cargo:rustc-link-lib=dylib=m");
    println!("cargo:rerun-if-changed=fortran/ablator.f90");
    println!("cargo:rerun-if-changed=c/aero.c");
}
