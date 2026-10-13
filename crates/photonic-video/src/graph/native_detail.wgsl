@group(0) @binding(0) var source:texture_2d<f32>;
@group(0) @binding(1) var blurred:texture_2d<f32>;
@group(0) @binding(2) var unused_sampler:sampler;
struct Params{ info:vec4<f32>, size:vec4<f32> }
@group(0) @binding(3) var<uniform> params:Params;
@fragment fn fs(@builtin(position) pos:vec4<f32>)->@location(0) vec4<f32>{
 if(pos.x>=params.size.x||pos.y>=params.size.y){return vec4<f32>(0.0);}
 let xy=vec2<i32>(pos.xy);let src=textureLoad(source,xy,0);if(src.a<=0.0){return vec4<f32>(0.0);}
 let low=textureLoad(blurred,xy,0);let straight=src.rgb/src.a;var diff=vec3<f32>(0.0);
 if(low.a>1e-8){diff=straight-low.rgb/low.a;}
 var value=straight;
 if(params.info.x==1.0){value=vec3<f32>(128.0/255.0)+diff;}
 else if(params.info.x==2.0){let luma=dot(straight,vec3<f32>(0.27222872,0.67408174,0.053689517));let bell=clamp(1.0-abs(luma-0.5)*2.0,0.0,1.0);value+=params.info.y*bell*diff;}
 else{let gate=clamp((abs(diff)-vec3<f32>(params.info.z))/(32.0/255.0),vec3<f32>(0.0),vec3<f32>(1.0));value+=params.info.y*gate*diff;}
 return vec4<f32>(value*src.a,src.a);
}
