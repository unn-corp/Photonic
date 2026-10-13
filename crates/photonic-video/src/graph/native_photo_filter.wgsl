@group(0) @binding(0) var source:texture_2d<f32>;
struct Params { info:vec4<f32>, a0:vec4<f32>,a1:vec4<f32>,a2:vec4<f32>,b0:vec4<f32>,b1:vec4<f32>,b2:vec4<f32>,tint:vec4<f32> }
@group(0) @binding(1) var<uniform> p:Params;
fn encode(v:f32)->f32{let magnitude=abs(v);if(magnitude==1.0){return v;}if(magnitude<=0.0031308){return v*12.92;}return sign(v)*(1.055*pow(magnitude,1.0/2.4)-0.055);}
fn decode(v:f32)->f32{let magnitude=abs(v);if(magnitude<=0.04045){return v/12.92;}return sign(v)*pow((magnitude+0.055)/1.055,2.4);}
@fragment fn fs(@builtin(position) pos:vec4<f32>)->@location(0) vec4<f32>{
 if(pos.x>=p.info.z||pos.y>=p.info.w){return vec4<f32>(0.0);}
 let src=textureLoad(source,vec2<i32>(pos.xy),0);if(src.a<=0.0){return vec4<f32>(0.0);}
 if(p.info.x==0.0||all(p.tint.xyz==vec3<f32>(1.0))){return src;}
 let straight=src.rgb/src.a;let linear=vec3<f32>(dot(p.a0.xyz,straight),dot(p.a1.xyz,straight),dot(p.a2.xyz,straight));let encoded=vec3<f32>(encode(linear.r),encode(linear.g),encode(linear.b));let tinted=encoded*p.tint.xyz;var rgb=encoded*(1.0-p.info.x)+tinted*p.info.x;if(p.info.y>0.5){let original=dot(encoded,vec3<f32>(0.299,0.587,0.114));let after=dot(rgb,vec3<f32>(0.299,0.587,0.114));if(after>0.0001){rgb*=original/after;}}
 let decoded=vec3<f32>(decode(rgb.r),decode(rgb.g),decode(rgb.b));let ap1=vec3<f32>(dot(p.b0.xyz,decoded),dot(p.b1.xyz,decoded),dot(p.b2.xyz,decoded));return vec4<f32>(ap1*src.a,src.a);
}
