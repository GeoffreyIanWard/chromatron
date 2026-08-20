// Instanced low-poly geometry (S12).
//
// One draw call covers every instance of a mesh: per-vertex data comes from the
// vertex buffer, per-instance transform from the instance buffer. That is what
// keeps the M1 gate's "under 20 draw calls for 100,000 instances" reachable.
//
// Shading is a single directional term with no texture. The palette atlas
// (S12) arrives with the asset pipeline at M3; until then a normal-based
// gradient is enough to tell whether geometry landed where it should, which is
// what the readback tests assert.

struct Camera {
    view_projection: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

// The shared palette. One texture, one bind group, for every instance of every
// mesh — which is the whole point of the technique (S12).
@group(1) @binding(0)
var palette: texture_2d<f32>;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    // Which palette column this part of the mesh reads (S12). The row comes
    // from the instance, so one mesh can have several materials and one draw
    // call can have several colour schemes.
    @location(2) palette_slot: u32,
};

// A 4x4 model matrix arrives as four vec4 attributes: WGSL vertex inputs cannot
// be matrices, so the rows are split across consecutive locations and
// reassembled here.
struct InstanceInput {
    @location(3) model_0: vec4<f32>,
    @location(4) model_1: vec4<f32>,
    @location(5) model_2: vec4<f32>,
    @location(6) model_3: vec4<f32>,
    @location(7) palette_row: u32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    // Flat, not interpolated: a palette coordinate is an index, and blending
    // two indices across a triangle produces a lookup into an entry neither
    // vertex asked for.
    @location(1) @interpolate(flat) palette_slot: u32,
    @location(2) @interpolate(flat) palette_row: u32,
};

@vertex
fn vs_main(vertex: VertexInput, instance: InstanceInput) -> VertexOutput {
    let model = mat4x4<f32>(
        instance.model_0,
        instance.model_1,
        instance.model_2,
        instance.model_3,
    );

    var out: VertexOutput;
    let world_position = model * vec4<f32>(vertex.position, 1.0);
    out.clip_position = camera.view_projection * world_position;

    // Normals are transformed by the model's rotation only. Uniform scale does
    // not change direction, and non-uniform scale would need the inverse
    // transpose — worth doing when scale stops being uniform, not before.
    out.world_normal = normalize((model * vec4<f32>(vertex.normal, 0.0)).xyz);
    out.palette_slot = vertex.palette_slot;
    out.palette_row = instance.palette_row;

    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // `textureLoad`, not a sampler. A palette must never blend between entries:
    // filtering across a slot boundary produces a colour in no palette at all,
    // and the artefact is a thin seam along a mesh's material edges that staring
    // at the texture does not explain.
    let base = textureLoad(
        palette,
        vec2<u32>(in.palette_slot, in.palette_row),
        0,
    );

    // A fixed key light from above and to the side, plus ambient. Enough to read
    // shape; the palette carries the colour, so this only modulates it.
    let light_direction = normalize(vec3<f32>(0.4, 0.8, 0.45));
    let lambert = max(dot(normalize(in.world_normal), light_direction), 0.0);
    let shade = 0.55 + 0.45 * lambert;

    return vec4<f32>(base.rgb * shade, base.a);
}
