@group(0) @binding(0) var source:texture_2d<f32>;
struct Params{info:vec4<f32>,gain:vec4<f32>,band:vec4<f32>,a0:vec4<f32>,a1:vec4<f32>,a2:vec4<f32>,b0:vec4<f32>,b1:vec4<f32>,b2:vec4<f32>}
@group(0) @binding(1) var<uniform> p:Params;
fn enc(v:f32)->f32{if(v<=0.0031308){return 12.92*v;}if(v==1.0){return 1.0;}return 1.055*pow(v,1.0/2.4)-0.055;}
fn dec(v:f32)->f32{if(v<=0.04045){return v/12.92;}if(v==1.0){return 1.0;}return pow((v+0.055)/1.055,2.4);}
fn correction(v:f32,gain:f32)->f32{if(gain==1.0){return 0.0;}let bound=clamp(v,0.0,1.0);let e=enc(bound);var g=gain;if(g>1.0){let t=clamp((e-0.75)/0.25,0.0,1.0);g=1.0+(g-1.0)*(1.0-t*t);}return dec(clamp(e*g,0.0,1.0))-bound;}
fn matrix(row:vec3<f32>,v:vec3<f32>)->f32{return v.g+row.r*(v.r-v.g)+row.b*(v.b-v.g);}
@fragment fn fs(@builtin(position) pos:vec4<f32>)->@location(0) vec4<f32>{
 if(pos.x>=p.info.z||pos.y>=p.info.w){return vec4<f32>(0.0);}
 let src=textureLoad(source,vec2<i32>(pos.xy),0);
 if(src.a<=0.000001||(all(p.gain.xyz==vec3<f32>(1.0))&&abs(p.band.y)<=0.000001)){return src;}
 var bg=1.0;if(abs(p.band.y)>0.000001){bg=clamp(1.0/(1.0+p.band.y*cos(6.283185307179586*p.band.x*floor(pos.y)/p.info.w+p.band.z)),0.5,2.0);}
 let base=src.rgb*bg/src.a;
 let linear=vec3<f32>(matrix(p.a0.xyz,base),matrix(p.a1.xyz,base),matrix(p.a2.xyz,base));
 let d=vec3<f32>(correction(linear.r,p.gain.x),correction(linear.g,p.gain.y),correction(linear.b,p.gain.z));
 let change=vec3<f32>(matrix(p.b0.xyz,d),matrix(p.b1.xyz,d),matrix(p.b2.xyz,d));
 return vec4<f32>(clamp((base+change)*src.a,vec3<f32>(-65504.0),vec3<f32>(65504.0)),src.a);
}
