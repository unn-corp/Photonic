@group(0) @binding(0) var source:texture_2d<f32>;
@group(0) @binding(1) var shadow:texture_2d<f32>;
@group(0) @binding(2) var unused_sampler:sampler;
struct Params { color:vec4<f32>, size:vec4<f32> }
@group(0) @binding(3) var<uniform> p:Params;
@fragment fn fs(@builtin(position) pos:vec4<f32>)->@location(0) vec4<f32>{
 if(pos.x>=p.size.x||pos.y>=p.size.y){return vec4<f32>(0.0);}
 let at=vec2<i32>(pos.xy);let src=textureLoad(source,at,0);let a=textureLoad(shadow,at,0).a*p.color.a*(1.0-src.a);
 return vec4<f32>(src.rgb+p.color.rgb*a,src.a+a);
}
