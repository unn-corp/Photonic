@group(0) @binding(0) var source: texture_2d<f32>;
struct Params { info: vec4<f32>, counts: vec4<f32> }
@group(0) @binding(1) var<uniform> params: Params;
fn sample_line(pos: vec2<f32>) -> vec4<f32> {
    let hi=vec2<i32>(params.info.zw)-vec2<i32>(1);
    let q=clamp(pos,vec2<f32>(0.0),vec2<f32>(hi));
    let lo=vec2<i32>(floor(q));let upper=min(lo+vec2<i32>(1),hi);let f=q-vec2<f32>(lo);
    let a=textureLoad(source,lo,0);let b=textureLoad(source,vec2<i32>(upper.x,lo.y),0);
    let c=textureLoad(source,vec2<i32>(lo.x,upper.y),0);let d=textureLoad(source,upper,0);
    return mix(mix(a,b,f.x),mix(c,d,f.x),f.y);
}
@fragment fn fs(@builtin(position) pos:vec4<f32>) -> @location(0) vec4<f32> {
    if (pos.x>=params.info.z || pos.y>=params.info.w) {return vec4<f32>(0.0);}
    let delta=params.info.xy;let xy=floor(pos.xy);
    if (all(delta==vec2<f32>(0.0))) {return sample_line(xy);}
    let steps=u32(params.counts.x);
    var sum=vec4<f32>(0.0);
    for(var i=0u;i<=steps;i++) {sum+=sample_line(xy+delta*(f32(i)/f32(steps)-0.5));}
    return sum/f32(steps+1u);
}
