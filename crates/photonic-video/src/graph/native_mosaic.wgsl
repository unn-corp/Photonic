@group(0) @binding(0) var source: texture_2d<f32>;
struct Params { info: vec4<u32> }
@group(0) @binding(1) var<uniform> params: Params;

// Reduction target has exactly ceil(logical size / cell size) pixels.
@fragment fn reduce(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let first = vec2<u32>(pos.xy) * params.info.xy;
    if (any(first >= params.info.zw)) { return vec4<f32>(0.0); }
    let end = min(first + params.info.xy, params.info.zw);
    var sum = vec4<f32>(0.0);
    for (var y = first.y; y < end.y; y++) {
        for (var x = first.x; x < end.x; x++) {
            sum += textureLoad(source, vec2<i32>(i32(x), i32(y)), 0);
        }
    }
    let n = (end.x-first.x)*(end.y-first.y);
    return sum / f32(max(n,1u));
}

@fragment fn expand(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let xy = vec2<u32>(pos.xy);
    if (any(xy >= params.info.zw)) { return vec4<f32>(0.0); }
    return textureLoad(source, vec2<i32>(xy / params.info.xy), 0);
}
