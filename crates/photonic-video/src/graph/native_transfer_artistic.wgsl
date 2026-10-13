@group(0) @binding(0) var source:texture_2d<f32>;
struct Params { info:vec4<f32>, a0:vec4<f32>,a1:vec4<f32>,a2:vec4<f32>,b0:vec4<f32>,b1:vec4<f32>,b2:vec4<f32> }
@group(0) @binding(1) var<uniform> p:Params;
fn encode(v:f32)->f32{let magnitude=abs(v);if(magnitude==1.0){return v;}if(magnitude<=0.0031308){return v*12.92;}return sign(v)*(1.055*pow(magnitude,1.0/2.4)-0.055);}
fn decode(v:f32)->f32{let magnitude=abs(v);if(magnitude<=0.04045){return v/12.92;}return sign(v)*pow((magnitude+0.055)/1.055,2.4);}
@fragment fn fs(@builtin(position) pos:vec4<f32>)->@location(0) vec4<f32>{
 if(pos.x>=p.info.z||pos.y>=p.info.w){return vec4<f32>(0.0);}
 let src=textureLoad(source,vec2<i32>(pos.xy),0);if(src.a<=0.0){return vec4<f32>(0.0);}
 let straight=src.rgb/src.a;var converted=vec3<f32>(dot(p.a0.xyz,straight),dot(p.a1.xyz,straight),dot(p.a2.xyz,straight));if(straight.r==straight.g&&straight.g==straight.b){converted=straight;}if(p.info.x==2.0&&straight.r==straight.g&&straight.g==straight.b){return src;}var linear=converted;if(p.info.x<2.0){linear=clamp(linear,vec3<f32>(0.0),vec3<f32>(1.0));}var rgb=vec3<f32>(encode(linear.r),encode(linear.g),encode(linear.b));
 if(p.info.x==0.0){let steps=p.info.y-1.0;rgb=floor(rgb*steps+vec3<f32>(0.5))/steps;}
 else if(p.info.x==1.0){let y=dot(rgb,vec3<f32>(0.299,0.587,0.114));rgb=vec3<f32>(select(0.0,1.0,y>=p.info.y));}
 else if(p.info.x==2.0){rgb=vec3<f32>((max(rgb.r,max(rgb.g,rgb.b))+min(rgb.r,min(rgb.g,rgb.b)))*0.5);}
 else{rgb=vec3<f32>(1.0)-rgb;}
 let decoded=vec3<f32>(decode(rgb.r),decode(rgb.g),decode(rgb.b));let ap1=vec3<f32>(dot(p.b0.xyz,decoded),dot(p.b1.xyz,decoded),dot(p.b2.xyz,decoded));var result=ap1*src.a;if(p.info.x==3.0){result=clamp(result,vec3<f32>(-65504.0),vec3<f32>(65504.0));}return vec4<f32>(result,src.a);
}
