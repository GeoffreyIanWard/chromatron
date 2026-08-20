// GPU frustum culling (S12).
//
// One thread per instance. A surviving instance copies itself into a compacted
// buffer, and the slot it lands in comes from an atomic increment of the
// indirect draw's instance count — so the same counter that decides where the
// data goes is the one the draw call reads. There is no second pass to reconcile
// them and no CPU round trip.
//
// The six inequalities here are the same ones `cx_render::culling` states in
// Rust. That duplication is deliberate and is checked: a test runs both over the
// same instances and compares the counts.

struct Cull {
    // Left, right, bottom, top, near, far — the order `Frustum::to_raw` emits.
    // Each is [nx, ny, nz, d] with the normal pointing into the visible side.
    planes: array<vec4<f32>, 6>,
    instance_count: u32,
    // Three scalars rather than a `vec3<u32>`. A `vec3` aligns to 16 in WGSL,
    // so it would sit at offset 112 rather than 100 and push the struct to 128
    // bytes while the Rust side has 112 — which wgpu reports as "bound with
    // size 112 where the shader expects 128", and which nothing else would have
    // caught.
    pad_a: u32,
    pad_b: u32,
    pad_c: u32,
};

struct Instance {
    model_0: vec4<f32>,
    model_1: vec4<f32>,
    model_2: vec4<f32>,
    model_3: vec4<f32>,
};

// Matches wgpu's DrawIndexedIndirectArgs, field for field. A mismatch here is
// not a compile error — it is a draw call reading the wrong word and asking for
// four billion instances.
struct DrawArgs {
    index_count: u32,
    instance_count: atomic<u32>,
    first_index: u32,
    base_vertex: i32,
    first_instance: u32,
};

@group(0) @binding(0) var<uniform> cull: Cull;
@group(0) @binding(1) var<storage, read> instances: array<Instance>;
@group(0) @binding(2) var<storage, read_write> visible: array<Instance>;
@group(0) @binding(3) var<storage, read_write> draw: DrawArgs;

// 64 threads per group. The dispatch rounds up, so the bounds check below is
// load-bearing rather than defensive: the last group is almost always partly
// past the end.
@compute @workgroup_size(64)
fn cull_instances(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x;
    if (index >= cull.instance_count) {
        return;
    }

    let instance = instances[index];
    let centre = instance.model_3.xyz;

    // A conservative bounding sphere for a unit cube under this transform: the
    // half-diagonal, scaled by the largest axis. Taking the largest rather than
    // each axis separately keeps it a sphere, and a sphere that is slightly too
    // big draws a few things it did not need to — which is the error worth
    // making, because the other one punches holes in the world.
    let scale_x = length(instance.model_0.xyz);
    let scale_y = length(instance.model_1.xyz);
    let scale_z = length(instance.model_2.xyz);
    let radius = 0.8660254 * max(scale_x, max(scale_y, scale_z));

    for (var plane = 0u; plane < 6u; plane = plane + 1u) {
        let equation = cull.planes[plane];
        let distance = dot(equation.xyz, centre) + equation.w;
        if (distance < -radius) {
            return;
        }
    }

    // Survived. The slot and the draw's instance count are the same number,
    // taken atomically, so they cannot disagree.
    let slot = atomicAdd(&draw.instance_count, 1u);
    visible[slot] = instance;
}
