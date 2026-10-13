@group(0) @binding(0) var source: texture_2d<f32>;
struct Params {
    info: vec4<f32>, // logical width, height, jump step, phase (0 = seed)
    scale: vec4<f32>,
}
@group(0) @binding(1) var<uniform> p: Params;

// Each byte is exactly representable in half storage, unlike large raw pixel
// coordinates. A negative first channel is the distinct no-seed sentinel.
fn pack(v: vec2<i32>) -> vec4<f32> {
    return vec4<f32>(f32(v.x % 256), f32(v.x / 256),
                     f32(v.y % 256), f32(v.y / 256));
}
fn unpack(v: vec4<f32>) -> vec2<i32> {
    return vec2<i32>(i32(v.x) + 256 * i32(v.y),
                     i32(v.z) + 256 * i32(v.w));
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let xy = vec2<i32>(pos.xy);
    let size = vec2<i32>(p.info.xy);
    let invalid = vec4<f32>(-1.0);
    if (any(xy >= size)) { return invalid; }

    if (p.info.w == 0.0) {
        if (textureLoad(source, xy, 0).a < 0.5) { return invalid; }
        var offsets = array<vec2<i32>, 4>(
            vec2<i32>(-1, 0), vec2<i32>(1, 0),
            vec2<i32>(0, -1), vec2<i32>(0, 1));
        for (var i = 0u; i < 4u; i++) {
            let q = xy + offsets[i];
            if (any(q < vec2<i32>(0)) || any(q >= size)) { return pack(xy); }
            if (textureLoad(source, q, 0).a < 0.5) { return pack(xy); }
        }
        return invalid;
    }

    let current = textureLoad(source, xy, 0);
    var best = unpack(current);
    var best_d = 3.402823e38;
    if (current.x >= 0.0) {
        let d = vec2<f32>(xy - best) / p.scale.xy;
        best_d = dot(d, d);
    }
    let step = i32(p.info.z);
    // Deterministic strict comparison/order agrees with the native CPU JFA.
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            if (dx == 0 && dy == 0) { continue; }
            let q = xy + vec2<i32>(dx, dy) * step;
            if (any(q < vec2<i32>(0)) || any(q >= size)) { continue; }
            let candidate = textureLoad(source, q, 0);
            if (candidate.x < 0.0) { continue; }
            let seed = unpack(candidate);
            let d = vec2<f32>(xy - seed) / p.scale.xy;
            let nd = dot(d, d);
            if (nd < best_d) { best_d = nd; best = seed; }
        }
    }
    if (best.x < 0) { return invalid; }
    return pack(best);
}
