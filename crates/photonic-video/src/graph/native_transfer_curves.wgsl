@group(0) @binding(0) var source:texture_2d<f32>;
struct Params { info:vec4<f32>, a0:vec4<f32>,a1:vec4<f32>,a2:vec4<f32>,b0:vec4<f32>,b1:vec4<f32>,b2:vec4<f32>,knots:array<vec4<f32>,5> }
@group(0) @binding(1) var<uniform> p:Params;
fn encode(v:f32)->f32{let magnitude=abs(v);if(magnitude==1.0){return v;}if(magnitude<=0.0031308){return v*12.92;}return sign(v)*(1.055*pow(magnitude,1.0/2.4)-0.055);}
fn decode(v:f32)->f32{let magnitude=abs(v);if(magnitude<=0.04045){return v/12.92;}return sign(v)*pow((magnitude+0.055)/1.055,2.4);}
fn curve(x:f32)->f32{let n=u32(p.info.x);if(n<2u){return x;}let first=p.knots[0];let last=p.knots[n-1u];if(x<=first.x){return first.y+(x-first.x)*first.z;}if(x>=last.x){return last.y+(x-last.x)*last.z;}for(var k=0u;k+1u<n;k++){let a=p.knots[k];let b=p.knots[k+1u];if(x>=a.x&&x<=b.x){let h=b.x-a.x;let t=(x-a.x)/h;let t2=t*t;let t3=t2*t;return(2.0*t3-3.0*t2+1.0)*a.y+(t3-2.0*t2+t)*h*a.z+(-2.0*t3+3.0*t2)*b.y+(t3-t2)*h*b.z;}}return last.y;}
@fragment fn fs(@builtin(position) pos:vec4<f32>)->@location(0) vec4<f32>{
 if(pos.x>=p.info.z||pos.y>=p.info.w){return vec4<f32>(0.0);}
 let src=textureLoad(source,vec2<i32>(pos.xy),0);if(src.a<=0.0){return vec4<f32>(0.0);}
 if(p.info.y>0.5){return src;}
 let straight=src.rgb/src.a;let linear=vec3<f32>(dot(p.a0.xyz,straight),dot(p.a1.xyz,straight),dot(p.a2.xyz,straight));let rgb=vec3<f32>(curve(encode(linear.r)),curve(encode(linear.g)),curve(encode(linear.b)));
 let decoded=vec3<f32>(decode(rgb.r),decode(rgb.g),decode(rgb.b));let ap1=vec3<f32>(dot(p.b0.xyz,decoded),dot(p.b1.xyz,decoded),dot(p.b2.xyz,decoded));return vec4<f32>(clamp(ap1*src.a,vec3<f32>(-65504.0),vec3<f32>(65504.0)),src.a);
}
