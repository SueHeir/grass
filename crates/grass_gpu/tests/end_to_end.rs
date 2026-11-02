//! Runs real kernels on whatever adapter this machine has, in a real schedule.
//!
//! Each test skips with a note when there is no adapter, so a machine without a GPU
//! still builds and tests the crate.

use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use grass_app::prelude::*;
use grass_gpu::{wgpu, Current, DualBuffer, GpuDevice, GpuPlugin};
use grass_scheduler::prelude::*;
use grass_scheduler::{CoherenceRegistry, MirrorBridge};

/// `out[i] = 2 * data[i]`, in place.
const DOUBLE: &str = r#"
@group(0) @binding(0) var<storage, read_write> data: array<f32>;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x < arrayLength(&data)) { data[id.x] = data[id.x] * 2.0; }
}
"#;

/// The device is shared by the whole process, and so are its transfer counts, so these
/// tests take turns. The guard goes with the device.
fn gpu_or_skip() -> Option<(GpuDevice, MutexGuard<'static, ()>)> {
    static TURN: Mutex<()> = Mutex::new(());
    let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    match GpuDevice::default_device() {
        Ok(device) => Some((device, turn)),
        Err(e) => {
            eprintln!("skipping: {e}");
            None
        }
    }
}

fn run_double(gpu: &GpuDevice, column: &mut DualBuffer<f32>) {
    let pipeline = gpu.compute_pipeline("double", DOUBLE, "main");
    let buffer = column.gpu_write();
    let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        }],
    });
    let groups = (column.len() as u32).div_ceil(64);
    gpu.submit_compute("double", |pass| {
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(groups, 1, 1);
    });
}

#[test]
fn kernel_matches_the_cpu_loop() {
    let Some((gpu, _turn)) = gpu_or_skip() else {
        return;
    };
    let input: Vec<f32> = (0..1000).map(|i| i as f32 * 0.25 - 40.0).collect();
    let cpu: Vec<f32> = input.iter().map(|x| x * 2.0).collect();
    let mut column = DualBuffer::new(&gpu, "x", wgpu::BufferUsages::STORAGE, input);
    run_double(&gpu, &mut column);
    // Doubling a float is exact, so the two paths must agree bit for bit.
    assert_eq!(column.host(), cpu.as_slice());
}

#[test]
fn a_chain_of_kernels_transfers_only_at_the_ends() {
    let Some((gpu, _turn)) = gpu_or_skip() else {
        return;
    };
    let mut column = DualBuffer::new(&gpu, "x", wgpu::BufferUsages::STORAGE, vec![1.0f32; 128]);
    let start = gpu.counts().snapshot();
    for _ in 0..4 {
        run_double(&gpu, &mut column);
    }
    assert_eq!(column.current(), Current::Gpu);
    let before_read = gpu.counts().snapshot().since(&start);
    assert_eq!((before_read.uploads, before_read.downloads), (1, 0));

    assert_eq!(column.host()[0], 16.0);
    let _ = column.host();
    let after_read = gpu.counts().snapshot().since(&start);
    assert_eq!((after_read.uploads, after_read.downloads), (1, 1));
}

#[test]
fn writing_one_side_first_brings_it_up_to_date() {
    let Some((gpu, _turn)) = gpu_or_skip() else {
        return;
    };
    let mut column = DualBuffer::new(&gpu, "x", wgpu::BufferUsages::STORAGE, vec![1.0f32; 64]);
    run_double(&gpu, &mut column);
    // A CPU write on stale data would lose the kernel's result in the other elements.
    column.host_mut()[0] = 10.0;
    assert_eq!(column.current(), Current::Cpu);
    assert_eq!(column.host()[1], 2.0);
    run_double(&gpu, &mut column);
    let host = column.host();
    assert_eq!((host[0], host[1]), (20.0, 4.0));
}

#[test]
fn overwriting_discards_the_side_it_replaces() {
    let Some((gpu, _turn)) = gpu_or_skip() else {
        return;
    };
    let mut column = DualBuffer::new(&gpu, "x", wgpu::BufferUsages::STORAGE, vec![1.0f32; 64]);
    let start = gpu.counts().snapshot();
    run_double(&gpu, &mut column);
    column.host_overwrite().fill(5.0);
    assert_eq!(column.host()[0], 5.0);
    let moved = gpu.counts().snapshot().since(&start);
    assert_eq!(
        moved.downloads, 0,
        "the kernel's result was replaced unread"
    );
}

#[test]
fn resize_keeps_data_and_says_when_to_rebind() {
    let Some((gpu, _turn)) = gpu_or_skip() else {
        return;
    };
    let mut column = DualBuffer::new(&gpu, "x", wgpu::BufferUsages::STORAGE, vec![1.0f32; 64]);
    run_double(&gpu, &mut column);
    let generation = column.generation();

    column.resize(32);
    assert_eq!(
        column.generation(),
        generation,
        "shrinking keeps the buffer"
    );
    column.resize(1000);
    assert!(
        column.generation() > generation,
        "growing replaces the buffer"
    );

    run_double(&gpu, &mut column);
    let host = column.host();
    assert_eq!(host.len(), 1000);
    assert_eq!((host[0], host[31], host[32]), (4.0, 4.0, 0.0));
}

#[test]
fn columns_download_together() {
    let Some((gpu, _turn)) = gpu_or_skip() else {
        return;
    };
    let mut a = DualBuffer::new(&gpu, "a", wgpu::BufferUsages::STORAGE, vec![1.0f32; 64]);
    let mut b = DualBuffer::new(&gpu, "b", wgpu::BufferUsages::STORAGE, vec![3.0f32; 32]);
    run_double(&gpu, &mut a);
    run_double(&gpu, &mut b);
    let start = gpu.counts().snapshot();
    DualBuffer::sync_all_to_host(&mut [&mut a, &mut b]);
    let moved = gpu.counts().snapshot().since(&start);
    assert_eq!((moved.downloads, moved.batches), (2, 1));
    assert_eq!((a.current(), b.current()), (Current::Both, Current::Both));
    assert_eq!((a.host()[0], b.host()[31]), (2.0, 6.0));
}

#[test]
fn resize_overwrite_downloads_nothing() {
    let Some((gpu, _turn)) = gpu_or_skip() else {
        return;
    };
    let mut column = DualBuffer::new(&gpu, "x", wgpu::BufferUsages::STORAGE, vec![1.0f32; 64]);
    run_double(&gpu, &mut column);
    let start = gpu.counts().snapshot();
    column.resize_overwrite(32);
    column.host_overwrite().fill(7.0);
    assert_eq!(column.host().len(), 32);
    assert_eq!(gpu.counts().snapshot().since(&start).downloads, 0);
}

#[test]
fn upload_range_touches_only_that_range() {
    let Some((gpu, _turn)) = gpu_or_skip() else {
        return;
    };
    let mut column = DualBuffer::new(&gpu, "x", wgpu::BufferUsages::STORAGE, vec![1.0f32; 64]);
    column.sync_to_gpu();
    column.upload_range(60, &[3.0; 4]);
    run_double(&gpu, &mut column);
    let host = column.host();
    assert_eq!((host[59], host[60], host[63]), (2.0, 6.0, 6.0));
}

#[test]
fn empty_column_is_legal() {
    let Some((gpu, _turn)) = gpu_or_skip() else {
        return;
    };
    let mut column = DualBuffer::<f32>::zeroed(&gpu, "x", 0);
    assert!(column.is_empty());
    let _ = column.gpu_write();
    assert!(column.host().is_empty());
}

// ── Sharing a device ─────────────────────────────────────────────────────────

#[test]
fn separate_apps_on_one_adapter_share_one_device() {
    let Some((gpu, _turn)) = gpu_or_skip() else {
        return;
    };
    let mut fluid = App::new();
    fluid.add_plugins(GpuPlugin::default());
    let mut solid = App::new();
    solid.add_plugins(GpuPlugin::on_adapter(gpu.adapter_index()));
    let fluid = fluid.get_resource_ref::<GpuDevice>().unwrap().clone();
    let solid = solid.get_resource_ref::<GpuDevice>().unwrap().clone();

    // A buffer from one App binds into a pipeline from the other, the case a coupled
    // transfer needs. On separate wgpu devices this would be a validation error.
    let mut column = DualBuffer::new(&solid, "x", wgpu::BufferUsages::STORAGE, vec![1.0f32; 8]);
    run_double(&fluid, &mut column);
    assert_eq!(column.host()[0], 2.0);
}

#[test]
fn libraries_reuse_the_first_device_and_a_custom_one_is_respected() {
    let Some((gpu, _turn)) = gpu_or_skip() else {
        return;
    };
    struct LibA;
    impl Plugin for LibA {
        fn build(&self, app: &mut App) {
            app.add_plugins(GpuPlugin::default());
        }
    }
    struct LibB;
    impl Plugin for LibB {
        fn build(&self, app: &mut App) {
            app.add_plugins(GpuPlugin::default());
        }
    }
    let mut app = App::new();
    app.init_shared_resource(|| gpu.clone());
    app.add_plugins(LibA).add_plugins(LibB);
    assert!(app.has_resource::<CoherenceRegistry>());
    assert_eq!(
        app.get_resource_ref::<GpuDevice>().unwrap().device,
        gpu.device
    );
}

#[test]
#[should_panic(expected = "already uses GPU adapter")]
fn asking_for_another_adapter_too_late_is_an_error() {
    if gpu_or_skip().is_none() {
        panic!("already uses GPU adapter (skipped: no adapter)");
    }
    let mut app = App::new();
    app.add_plugins(GpuPlugin::default());
    app.add_plugins(GpuPlugin::on_adapter(99));
}

// ── CPU systems that know nothing about the GPU ──────────────────────────────

/// An ordinary resource, as a library's CPU systems see it.
struct Particles {
    x: Vec<f32>,
}

struct GpuColumn(DualBuffer<f32>);

struct Seen(Vec<f32>);

/// Downloads the column into the ordinary resource when the scheduler asks.
struct ParticlesBridge {
    particles: usize,
    column: usize,
}

impl MirrorBridge for ParticlesBridge {
    fn resolve(&mut self, index: &HashMap<TypeId, usize>) {
        self.particles = index[&TypeId::of::<Particles>()];
        self.column = index[&TypeId::of::<GpuColumn>()];
    }

    fn download(&self, resources: &[RefCell<Box<dyn Any>>]) {
        let mut column = resources[self.column].borrow_mut();
        let column = column.downcast_mut::<GpuColumn>().unwrap();
        let mut particles = resources[self.particles].borrow_mut();
        let particles = particles.downcast_mut::<Particles>().unwrap();
        particles.x.copy_from_slice(column.0.host());
    }
}

#[derive(Debug, Clone, Copy, grass_derive::ScheduleSet)]
enum Phase {
    Kernels,
    Output,
}

/// Runs on the GPU and says so. It takes the registry, which marks it as managing
/// its own coherence, so the scheduler leaves it alone.
fn gpu_step(
    gpu: Res<GpuDevice>,
    mut column: ResMut<GpuColumn>,
    mut coh: ResMut<CoherenceRegistry>,
) {
    run_double(&gpu, &mut column.0);
    coh.mark_device_dirty(TypeId::of::<Particles>());
}

/// An ordinary CPU system. It has never heard of a GPU.
fn cpu_reader(particles: Res<Particles>, mut seen: ResMut<Seen>) {
    seen.0 = particles.x.clone();
}

#[test]
fn scheduler_downloads_before_a_cpu_system_reads() {
    let Some((gpu, _turn)) = gpu_or_skip() else {
        return;
    };
    let mut app = App::new();
    app.add_plugins(GpuPlugin::default());
    app.add_resource(Particles { x: vec![1.0; 64] });
    app.add_resource(GpuColumn(DualBuffer::new(
        &gpu,
        "x",
        wgpu::BufferUsages::STORAGE,
        vec![1.0; 64],
    )));
    app.add_resource(Seen(Vec::new()));
    {
        let mut coh = app
            .get_mut_resource(TypeId::of::<CoherenceRegistry>())
            .unwrap()
            .borrow_mut();
        coh.downcast_mut::<CoherenceRegistry>().unwrap().register(
            TypeId::of::<Particles>(),
            Box::new(ParticlesBridge {
                particles: 0,
                column: 0,
            }),
        );
    }
    app.add_update_system(gpu_step, Phase::Kernels);
    app.add_update_system(cpu_reader, Phase::Output);
    app.organize_systems();

    let before = gpu.counts().snapshot();
    for _ in 0..3 {
        app.run();
    }
    let moved = gpu.counts().snapshot().since(&before);

    // Each of the three updates doubled on the GPU, and the reader saw it each time.
    let seen = app.get_resource_ref::<Seen>().unwrap();
    assert_eq!(seen.0, vec![8.0; 64]);
    assert_eq!(moved.downloads, 3);
    let coh = app.get_resource_ref::<CoherenceRegistry>().unwrap();
    assert_eq!(coh.syncs(TypeId::of::<Particles>()), 3);
}
