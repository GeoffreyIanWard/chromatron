//! GPU frustum culling, checked against the Rust implementation of the same
//! rule (S12/M1).
//!
//! The six inequalities exist twice: in `cx_render::culling` and in
//! `shaders/cull.wgsl`. That duplication is unavoidable — one has to run on each
//! processor — so what matters is that a disagreement is *caught*. These tests
//! run both over the same instances and compare.
//!
//! A shader that culled slightly differently would be invisible in ordinary use:
//! the picture would look right from most angles and quietly lose geometry from
//! some.

use cx_core::math::{Mat4, Quat, Vec3};
use cx_render::testing::device_or_skip;
use cx_render::{Camera, CullPass, Frustum, InstancedRenderer, MeshData, RenderDevice};

/// How many instances the fixtures use.
const COUNT: u32 = 4_096;

/// Instances spread over a cube of space, deterministically.
///
/// Positional rather than random: a failure has to be reproducible, and a seeded
/// RNG would be one more thing that could differ between the two sides.
fn scattered(count: u32) -> Vec<cx_view::ExtractedInstance> {
    (0..count)
        .map(|index| {
            let angle = index as f32 * 0.61;
            let radius = (index % 200) as f32;
            cx_view::ExtractedInstance {
                position: Vec3::new(
                    angle.cos() * radius,
                    ((index % 40) as f32) - 20.0,
                    angle.sin() * radius,
                ),
                rotation: Quat::IDENTITY,
                scale: Vec3::ONE,
            }
        })
        .collect()
}

/// The count the Rust frustum keeps, using the same bounding sphere the shader
/// derives from the model matrix.
fn cpu_visible(frustum: Frustum, instances: &[cx_view::ExtractedInstance]) -> u32 {
    instances
        .iter()
        .filter(|instance| {
            let model = Mat4::from_scale_rotation_translation(
                instance.scale,
                instance.rotation,
                instance.position,
            );
            let columns = model.to_cols_array_2d();
            let axis = |column: [f32; 4]| Vec3::new(column[0], column[1], column[2]).length();
            let radius = 0.866_025_4 * axis(columns[0]).max(axis(columns[1]).max(axis(columns[2])));

            frustum.intersects_sphere(instance.position, radius)
        })
        .count() as u32
}

/// The aspect ratio every camera here uses.
const ASPECT: f32 = 16.0 / 9.0;

struct Fixture {
    device: RenderDevice,
    renderer: InstancedRenderer,
    pass: CullPass,
}

impl Fixture {
    /// Runs the GPU cull and returns how many instances survived.
    fn gpu_visible(&self, camera: &Camera, instances: &[cx_view::ExtractedInstance]) -> u32 {
        self.pass
            .debug_cull_count(&self.device, &self.renderer, camera, ASPECT, instances)
    }
}

fn fixture(capacity: u32) -> Option<Fixture> {
    let device = device_or_skip()?;
    let renderer = InstancedRenderer::new(&device, &MeshData::unit_cube())
        .expect("the unit cube is a valid mesh");
    let pass = CullPass::new(&device, capacity);
    Some(Fixture {
        device,
        renderer,
        pass,
    })
}

/// **The check the whole design rests on:** the shader and the Rust
/// implementation keep the same instances.
#[test]
fn the_shader_agrees_with_the_rust_frustum() {
    let Some(fixture) = fixture(COUNT) else {
        return;
    };
    let instances = scattered(COUNT);

    // Several camera positions, because a single one can agree by luck — a
    // shader that ignored, say, the top plane would still match a camera whose
    // scene happens to sit below it.
    for (position, target) in [
        (Vec3::new(0.0, 30.0, 120.0), Vec3::ZERO),
        (Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0)),
        (Vec3::new(-80.0, 10.0, -80.0), Vec3::new(20.0, 0.0, 20.0)),
        (Vec3::new(0.0, 200.0, 0.0), Vec3::ZERO),
        (Vec3::new(500.0, 0.0, 500.0), Vec3::new(499.0, 0.0, 499.0)),
    ] {
        let camera = Camera::looking_at(position, target);
        let frustum = Frustum::from_view_projection(camera.view_projection(ASPECT));

        let expected = cpu_visible(frustum, &instances);
        let actual = fixture.gpu_visible(&camera, &instances);

        assert_eq!(
            actual, expected,
            "from {position:?} looking at {target:?}: the shader kept {actual} instances \
             and the Rust frustum kept {expected}"
        );
    }
}

#[test]
fn culling_actually_removes_something() {
    // The test above would pass if both sides kept everything. This is the
    // control: a camera pointed away from the scene must reject most of it, and
    // one pointed at it must keep more.
    let Some(fixture) = fixture(COUNT) else {
        return;
    };
    let instances = scattered(COUNT);

    let looking_at = Camera::looking_at(Vec3::new(0.0, 20.0, 400.0), Vec3::ZERO);
    let looking_away = Camera::looking_at(Vec3::new(0.0, 20.0, 400.0), Vec3::new(0.0, 20.0, 900.0));

    let kept = fixture.gpu_visible(&looking_at, &instances);
    let rejected = fixture.gpu_visible(&looking_away, &instances);

    assert!(kept > 0, "a camera facing the scene should keep something");
    assert!(
        rejected < kept / 4,
        "a camera facing away should reject most of the scene: kept {kept}, \
         rejected-view kept {rejected}"
    );
}

#[test]
fn the_count_resets_between_frames() {
    // The counter is what the indirect draw reads. Leaving last frame's value in
    // it makes the scene draw more instances every frame until it reads past the
    // end of the buffer — which is a validation error at best.
    let Some(fixture) = fixture(COUNT) else {
        return;
    };
    let instances = scattered(COUNT);
    let camera = Camera::looking_at(Vec3::new(0.0, 30.0, 120.0), Vec3::ZERO);

    let first = fixture.gpu_visible(&camera, &instances);

    for _ in 0..4 {
        let again = fixture.gpu_visible(&camera, &instances);
        assert_eq!(again, first, "the survivor count accumulated across frames");
    }
}

#[test]
fn an_empty_scene_culls_to_nothing() {
    let Some(fixture) = fixture(64) else {
        return;
    };
    let camera = Camera::looking_at(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO);

    assert_eq!(fixture.gpu_visible(&camera, &[]), 0);
}

#[test]
fn more_instances_than_capacity_are_clamped_rather_than_overrunning() {
    // The shader writes into a fixed buffer at an index from an atomic. Handing
    // it more instances than the buffer holds would let it write past the end,
    // which is memory corruption on the GPU rather than a validation error.
    let Some(fixture) = fixture(64) else {
        return;
    };
    let instances = scattered(1_000);
    let camera = Camera::looking_at(Vec3::new(0.0, 30.0, 400.0), Vec3::ZERO);

    let survived = fixture.gpu_visible(&camera, &instances);

    assert!(
        survived <= fixture.pass.capacity(),
        "{survived} instances survived into a buffer holding {}",
        fixture.pass.capacity()
    );
}

/// A count near the workgroup boundary, where a rounding error shows up.
///
/// The dispatch rounds up to whole workgroups, so the last one runs threads past
/// the end of the instance list. The shader's bounds check is what stops them,
/// and a count that is an exact multiple of the workgroup size would never
/// exercise it.
#[test]
fn counts_around_the_workgroup_boundary_are_handled() {
    let Some(fixture) = fixture(512) else {
        return;
    };
    let camera = Camera::looking_at(Vec3::new(0.0, 30.0, 400.0), Vec3::ZERO);

    for count in [1, 63, 64, 65, 127, 128, 129] {
        let instances = scattered(count);
        let frustum = Frustum::from_view_projection(camera.view_projection(ASPECT));

        let expected = cpu_visible(frustum, &instances);
        let actual = fixture.gpu_visible(&camera, &instances);

        assert_eq!(actual, expected, "with {count} instances");
    }
}
