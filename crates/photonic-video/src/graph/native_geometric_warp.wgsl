@group(0) @binding(0) var source:texture_2d<f32>;
struct Params { info:vec4<f32>, shape:vec4<f32> }
@group(0) @binding(1) var<uniform> params:Params;
fn sample_warp(center:vec2<f32>)->vec4<f32>{
 let hi=vec2<i32>(params.info.zw)-vec2<i32>(1);
 let q=clamp(center-vec2<f32>(0.5),vec2<f32>(0.0),vec2<f32>(hi));
 let lo=vec2<i32>(floor(q));let upper=min(lo+vec2<i32>(1),hi);let f=q-vec2<f32>(lo);
 let a=textureLoad(source,lo,0);let b=textureLoad(source,vec2<i32>(upper.x,lo.y),0);
 let c=textureLoad(source,vec2<i32>(lo.x,upper.y),0);let d=textureLoad(source,upper,0);
 return mix(mix(a,b,f.x),mix(c,d,f.x),f.y);
}
@fragment fn fs(@builtin(position) pos:vec4<f32>)->@location(0) vec4<f32>{
 if(pos.x>=params.info.z || pos.y>=params.info.w){return vec4<f32>(0.0);}
 let amount=params.info.y;
 if(amount==0.0){return textureLoad(source,vec2<i32>(pos.xy),0);}
 let scale=params.shape.yz;let size=params.info.zw/scale;
 let center=size*0.5;let radius=min(size.x,size.y)*0.5;
 let p=pos.xy/scale;var q=p;
 if(params.info.x==2.0){
   let phase=6.283185307179586;
   q+=amount*sin(phase*fract(p.yx/params.shape.x));
 }else{
   let delta=p-center;let distance=length(delta);
   if(distance>0.0 && distance<radius){
     let n=distance/radius;var factor=1.0;
     if(params.info.x==0.0){factor=1.0+amount*(1.0-n)*(1.0-n);}
     else if(amount>=0.0){factor=1.0+amount*(asin(n)/1.5707963267948966/n-1.0);}
     else{factor=1.0+(-amount)*(sin(n*1.5707963267948966)/n-1.0);}
     q=center+delta*factor;
   }
 }
 return sample_warp(q*scale);
}
