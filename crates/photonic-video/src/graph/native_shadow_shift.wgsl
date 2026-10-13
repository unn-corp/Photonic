@group(0) @binding(0) var source:texture_2d<f32>;
struct Params { info:vec4<f32> }
@group(0) @binding(1) var<uniform> p:Params;
@fragment fn fs(@builtin(position) pos:vec4<f32>)->@location(0) vec4<f32>{
 if(pos.x>=p.info.z||pos.y>=p.info.w){return vec4<f32>(0.0);}
 let hi=vec2<i32>(p.info.zw)-vec2<i32>(1);let q=clamp(pos.xy-p.info.xy-vec2<f32>(0.5),vec2<f32>(0.0),vec2<f32>(hi));let lo=vec2<i32>(floor(q));let up=min(lo+vec2<i32>(1),hi);let f=q-vec2<f32>(lo);
 let a=mix(mix(textureLoad(source,lo,0).a,textureLoad(source,vec2<i32>(up.x,lo.y),0).a,f.x),mix(textureLoad(source,vec2<i32>(lo.x,up.y),0).a,textureLoad(source,up,0).a,f.x),f.y);
 return vec4<f32>(0.0,0.0,0.0,a);
}
