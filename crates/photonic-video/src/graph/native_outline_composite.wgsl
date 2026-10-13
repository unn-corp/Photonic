@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var seeds: texture_2d<f32>;
@group(0) @binding(2) var unused_sampler: sampler;
struct Params {
    info: vec4<f32>, // logical width, height, authored thickness, opacity
    col: vec4<f32>,  // linear AP1 swatch
    scale: vec4<f32>,
}
@group(0) @binding(3) var<uniform> p: Params;

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let xy = vec2<i32>(pos.xy);
    if (pos.x >= p.info.x || pos.y >= p.info.y) { return vec4<f32>(0.0); }
    let src = textureLoad(source, xy, 0);
    if (p.info.z == 0.0 || p.info.w == 0.0) { return src; }

    var band = 1.0;
    if (src.a < 0.5) {
        let packed = textureLoad(seeds, xy, 0);
        // A fully empty field must not fabricate coverage at large thickness.
        if (packed.x < 0.0) { return src; }
        let seed = vec2<i32>(i32(packed.x) + 256 * i32(packed.y),
                             i32(packed.z) + 256 * i32(packed.w));
        let d = vec2<f32>(xy - seed) / p.scale.xy;
        // Solid band with a one-authored-pixel Hermite AA half-width.
        let t = clamp((length(d) - (p.info.z - 1.0)) * 0.5, 0.0, 1.0);
        band = 1.0 - t * t * (3.0 - 2.0 * t);
    }
    let a = band * p.info.w * (1.0 - src.a);
    return vec4<f32>(
        clamp(src.rgb + p.col.rgb * a, vec3<f32>(-65504.0), vec3<f32>(65504.0)),
        src.a + a);
}
