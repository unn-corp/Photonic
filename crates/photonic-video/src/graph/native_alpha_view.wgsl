@group(0) @binding(0) var source:texture_2d<f32>;
struct Params{info:vec4<f32>}
@group(0) @binding(1) var<uniform> p:Params;
@fragment fn fs(@builtin(position) pos:vec4<f32>)->@location(0) vec4<f32>{if(pos.x>=p.info.y||pos.y>=p.info.z){return vec4<f32>(0.0);}let src=textureLoad(source,vec2<i32>(pos.xy),0);let mode=u32(p.info.x);if(mode==1u){return src;}let a=clamp(src.a,0.0,1.0);if(mode==2u){if(a>0.000001){return vec4<f32>(clamp(src.rgb/a,vec3<f32>(-65504.0),vec3<f32>(65504.0)),1.0);}return vec4<f32>(0.0,0.0,0.0,1.0);}return vec4<f32>(a,a,a,1.0);}
