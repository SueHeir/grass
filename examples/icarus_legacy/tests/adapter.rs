//! Adapter pieces that need no MPI: the station <-> face transfer and the
//! namelist override writer. (The Fortran code itself is exercised by the
//! `replay` and `entry3d` binaries under mpirun; see the README.)

use icarus_legacy::{write_overrides, StationMap, SurfaceFaces};

fn faces(theta_deg: &[f64], area: &[f64]) -> SurfaceFaces {
    SurfaceFaces {
        gid: (1..=theta_deg.len() as i32).collect(),
        theta: theta_deg.iter().map(|d| d.to_radians()).collect(),
        phi: vec![0.0; theta_deg.len()],
        area: area.to_vec(),
    }
}

#[test]
fn stations_to_faces_is_linear_and_clamped() {
    let map = StationMap::new(
        &[0.0, 20.0, 40.0],
        &faces(&[-1.0, 5.0, 20.0, 30.0, 50.0], &[1.0; 5]),
    );
    let f = map.to_faces(&[100.0, 200.0, 400.0]);
    let want = [100.0, 125.0, 200.0, 300.0, 400.0];
    for (a, b) in f.iter().zip(want) {
        assert!((a - b).abs() < 1e-12, "{f:?}");
    }
}

#[test]
fn faces_to_stations_preserves_a_uniform_field_across_ranks() {
    let stations = [0.0, 20.0, 40.0];
    // the same five faces split over two "ranks"
    let a = StationMap::new(&stations, &faces(&[5.0, 15.0], &[1.0, 2.0]));
    let b = StationMap::new(&stations, &faces(&[25.0, 35.0, 39.0], &[3.0, 1.0, 0.5]));
    let mut sums = a.partial_sums(&[0.7, 0.7]);
    for (s, t) in sums.iter_mut().zip(b.partial_sums(&[0.7, 0.7, 0.7])) {
        *s += t;
    }
    for m in StationMap::finish(&sums) {
        assert!((m - 0.7).abs() < 1e-12);
    }
}

#[test]
fn overrides_absolutise_paths_and_redirect_output() {
    let dir = std::env::temp_dir().join(format!("icarus_legacy_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let deck = dir.join("case.nml");
    std::fs::write(
        &deck,
        "&control\n  case_name = 'x', env_file = 'env.dat' ! the table\n  bprime_file = '/abs/bprime.dat'\n/\n",
    )
    .unwrap();
    let set: toml::Table = toml::from_str(
        "[boundary]\nbc_source = \"external\"\nblowing_correction = false\n[control]\ndt_max = 0.025\n",
    )
    .unwrap();
    let ov = dir.join("ov.nml");
    write_overrides(&deck, &set, &dir.join("out"), &ov).unwrap();
    let text = std::fs::read_to_string(&ov).unwrap();
    assert!(
        text.contains("&boundary\n  bc_source = 'external'\n  blowing_correction = .false.\n/"),
        "{text}"
    );
    assert!(text.contains("dt_max = 2.5d-2"), "{text}");
    assert!(
        text.contains(&format!("env_file = '{}'", dir.join("env.dat").display())),
        "{text}"
    );
    assert!(text.contains("bprime_file = '/abs/bprime.dat'"), "{text}");
    assert!(
        text.contains(&format!("output_dir = '{}'", dir.join("out").display())),
        "{text}"
    );
    std::fs::remove_dir_all(&dir).ok();
}
