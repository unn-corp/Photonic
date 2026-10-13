@group(0) @binding(0) var source: texture_2d<f32>;
struct Params { info: vec4<u32> }
@group(0) @binding(1) var<uniform> params: Params;
@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let xy = vec2<i32>(pos.xy);
    let size = vec2<i32>(params.info.zw);
    if (any(xy >= size)) { return vec4<f32>(0.0); }
    let radius = i32(params.info.x);
    let direction = select(vec2<i32>(1,0),vec2<i32>(0,1),params.info.y==1u);
    var sum=vec4<f32>(0.0);
    for (var offset=-radius; offset<=radius; offset++) {
        let p=clamp(xy+offset*direction,vec2<i32>(0),size-vec2<i32>(1));
        sum+=textureLoad(source,p,0);
    }
    return sum/f32(2*radius+1);
}
