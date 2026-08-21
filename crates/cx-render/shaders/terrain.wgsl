// Terrain chunks (S07/M2).
//
// One draw per resident chunk mesh. Vertices are chunk-local; the instance
// buffer supplies the chunk's offset from the floating origin, so a chunk's
// mesh is uploaded once and never touched as the camera moves.
//
// Colouring is a placeholder: height bands blended by slope, plus the same
// single directional light the instanced shader uses. Real surface materials
// arrive with biomes (S07 steps 8–9); until then the bands exist to make
// elevation and erosion readable at a glance, which is what the demo is for.

struct Camera {
    view_projection: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

struct VertexInput {
    // Chunk-local position: x/z in 0..=512, y is absolute elevation in metres.
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

struct InstanceInput {
    // The chunk's corner relative to the floating origin, metres. The fourth
    // component pads the stride to 16 bytes and is ignored.
    @location(2) offset: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) elevation: f32,
};

@vertex
fn vs_main(vertex: VertexInput, instance: InstanceInput) -> VertexOutput {
    var out: VertexOutput;
    let world_position = vertex.position + instance.offset.xyz;
    out.clip_position = camera.view_projection * vec4<f32>(world_position, 1.0);
    out.world_normal = vertex.normal;
    out.elevation = vertex.position.y;
    return out;
}

// Slope carries the material until biomes exist. Absolute elevation cannot:
// the continental map moves a whole block's base height by hundreds of
// metres, so any fixed band paints some regions entirely one colour — the
// first render of real terrain came out all snow. Height contributes faint
// contour lines instead, so valleys and ridges still read at a glance.
const CONTOUR_SPACING: f32 = 50.0;
const SNOW_FROM: f32 = 1000.0;
const SNOW_TO: f32 = 1300.0;

const GRASS: vec3<f32> = vec3<f32>(0.18, 0.34, 0.12);
const ROCK: vec3<f32> = vec3<f32>(0.32, 0.28, 0.24);
const SNOW: vec3<f32> = vec3<f32>(0.86, 0.87, 0.90);

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let normal = normalize(in.world_normal);

    // Steep faces read as rock: carved valley walls and eroded gullies are
    // the terrain features worth seeing.
    let steepness = 1.0 - smoothstep(0.55, 0.85, normal.y);
    var base = mix(GRASS, ROCK, steepness);

    // A thin darkening at every contour boundary.
    let band = fract(in.elevation / CONTOUR_SPACING);
    let edge = smoothstep(0.0, 0.12, band) * (1.0 - smoothstep(0.88, 1.0, band));
    base = base * (0.88 + 0.12 * edge);

    // Snow only where terrain is genuinely alpine.
    base = mix(base, SNOW, smoothstep(SNOW_FROM, SNOW_TO, in.elevation));

    // The same key light as the instanced shader, so the two passes agree on
    // where the sun is.
    let light_direction = normalize(vec3<f32>(0.4, 0.8, 0.45));
    let lambert = max(dot(normal, light_direction), 0.0);
    let shade = 0.35 + 0.65 * lambert;

    return vec4<f32>(base * shade, 1.0);
}

// ---- Water ----
//
// Translucent surfaces over the opaque terrain: lake plates at the fill
// level, ribbons lying in carved channels. Depth is per-vertex, from
// worldgen's water grid; it drives both the colour (shallow teal to deep
// blue) and the alpha, which fades to nothing at the shoreline so the edge
// is a soft meeting rather than a hard polygon.

struct WaterVertexInput {
    // Chunk-local x/z, absolute water-surface elevation in y.
    @location(0) position: vec3<f32>,
    // Water depth at this vertex, metres. Zero right at the shore.
    @location(1) depth: f32,
};

struct WaterVertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) depth: f32,
};

@vertex
fn vs_water(vertex: WaterVertexInput, instance: InstanceInput) -> WaterVertexOutput {
    var out: WaterVertexOutput;
    let world_position = vertex.position + instance.offset.xyz;
    out.clip_position = camera.view_projection * vec4<f32>(world_position, 1.0);
    out.depth = vertex.depth;
    return out;
}

const SHALLOW_WATER: vec3<f32> = vec3<f32>(0.10, 0.30, 0.35);
const DEEP_WATER: vec3<f32> = vec3<f32>(0.02, 0.08, 0.20);

@fragment
fn fs_water(in: WaterVertexOutput) -> @location(0) vec4<f32> {
    let colour = mix(SHALLOW_WATER, DEEP_WATER, clamp(in.depth / 3.0, 0.0, 1.0));
    // Fade out over the last 20 cm so shorelines feather instead of cutting.
    let shore = smoothstep(0.0, 0.2, in.depth);
    let alpha = shore * clamp(0.35 + 0.30 * in.depth, 0.35, 0.85);
    return vec4<f32>(colour, alpha);
}
