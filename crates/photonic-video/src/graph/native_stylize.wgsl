@group(0) @binding(0) var source:texture_2d<f32>;
struct Params{info:vec4<f32>,scale:vec4<f32>}
@group(0) @binding(1) var<uniform> params:Params;
fn sample_straight(center:vec2<f32>)->vec3<f32>{
 let hi=vec2<i32>(params.info.zw)-vec2<i32>(1);let q=clamp(center-vec2<f32>(0.5),vec2<f32>(0.0),vec2<f32>(hi));let lo=vec2<i32>(floor(q));let upper=min(lo+vec2<i32>(1),hi);let f=q-vec2<f32>(lo);
 let a=textureLoad(source,lo,0);let b=textureLoad(source,vec2<i32>(upper.x,lo.y),0);let c=textureLoad(source,vec2<i32>(lo.x,upper.y),0);let d=textureLoad(source,upper,0);let p=mix(mix(a,b,f.x),mix(c,d,f.x),f.y);
 if(p.a>1e-8){return p.rgb/p.a;}return vec3<f32>(0.0);
}
@fragment fn fs(@builtin(position) pos:vec4<f32>)->@location(0) vec4<f32>{
 if(pos.x>=params.info.z||pos.y>=params.info.w){return vec4<f32>(0.0);}
 let src=textureLoad(source,vec2<i32>(pos.xy),0);let kind=params.info.x;let amount=params.info.y;
 if(kind==2.0&&amount==0.0){return src;}if(src.a<=0.0){return vec4<f32>(0.0);}
 let scale=params.scale.xy;
 if(kind==2.0){let size=params.info.zw/scale;let center=size*0.5;let max_dist=max(length((size-vec2<f32>(1.0))*0.5),0.0001);let authored=pos.xy/scale;let offset=(authored-center)*(amount/max_dist);let red=sample_straight((authored+offset)*scale);let blue=sample_straight((authored-offset)*scale);return vec4<f32>(red.r*src.a,src.g,blue.b*src.a,src.a);}
 var emboss=array<f32,9>(-1.0,-1.0,0.0,-1.0,0.0,1.0,0.0,1.0,1.0);
 var sx=array<f32,9>(-1.0,0.0,1.0,-2.0,0.0,2.0,-1.0,0.0,1.0);var sy=array<f32,9>(-1.0,-2.0,-1.0,0.0,0.0,0.0,1.0,2.0,1.0);
 var gx=0.0;var gy=0.0;
 for(var y=0u;y<3u;y++){for(var x=0u;x<3u;x++){let p=pos.xy+(vec2<f32>(f32(x),f32(y))-vec2<f32>(1.0))*scale;let l=dot(sample_straight(p),vec3<f32>(0.27222872,0.67408174,0.053689517));let i=y*3u+x;if(kind==0.0){gx+=l*emboss[i];}else{gx+=l*sx[i];gy+=l*sy[i];}}}
 var gray=length(vec2<f32>(gx,gy));if(kind==0.0){gray=128.0/255.0+gx;}return vec4<f32>(vec3<f32>(gray*src.a),src.a);
}
