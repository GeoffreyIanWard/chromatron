// Debug line rendering (S14).
//
// Deliberately the simplest shader in the engine: positions arrive already in
// extract space, colours come straight from the vertex, and nothing is lit.
// Debug geometry is read for *where* it is, not for what it looks like, and
// shading a wireframe makes it harder to read rather than easier.

struct Camera {
    view_projection: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

struct VertexInput {
    @location(0) position: vec3<f32>,
    // Declared Unorm8x4 in the vertex layout: the buffer carries four bytes,
    // the shader sees four floats in 0..1, and the conversion is free.
    @location(1) colour: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) colour: vec4<f32>,
};

@vertex
fn vs_main(vertex: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = camera.view_projection * vec4<f32>(vertex.position, 1.0);
    out.colour = vertex.colour;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.colour;
}
