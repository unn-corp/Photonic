@group(0) @binding(0) var source:texture_2d<f32>;
struct Params { row0:vec4<f32>, row1:vec4<f32>, row2:vec4<f32>, size:vec4<f32> }
@group(0) @binding(1) var<uniform> p:Params;
fn sample_at(center:vec2<f32>)->vec4<f32>{
 let hi=vec2<i32>(p.size.xy)-vec2<i32>(1);let q=clamp(center-vec2<f32>(0.5),vec2<f32>(0.0),vec2<f32>(hi));let lo=vec2<i32>(floor(q));let upper=min(lo+vec2<i32>(1),hi);let f=q-vec2<f32>(lo);
 return mix(mix(textureLoad(source,lo,0),textureLoad(source,vec2<i32>(upper.x,lo.y),0),f.x),mix(textureLoad(source,vec2<i32>(lo.x,upper.y),0),textureLoad(source,upper,0),f.x),f.y);
}
@fragment fn fs(@builtin(position) pos:vec4<f32>)->@location(0) vec4<f32>{
 if(pos.x>=p.size.x||pos.y>=p.size.y){return vec4<f32>(0.0);}
 if(all(p.row0.xyz==vec3<f32>(1.0,0.0,0.0))&&all(p.row1.xyz==vec3<f32>(0.0,1.0,0.0))&&all(p.row2.xyz==vec3<f32>(0.0,0.0,1.0))){return textureLoad(source,vec2<i32>(pos.xy),0);}
 let q=vec3<f32>(pos.xy/p.size.xy,1.0);let d=dot(p.row2.xyz,q);if(abs(d)<0.000001){return vec4<f32>(0.0);}
 let uv=vec2<f32>(dot(p.row0.xyz,q),dot(p.row1.xyz,q))/d;
 if(any(uv<vec2<f32>(0.0))||any(uv>=vec2<f32>(1.0))){return vec4<f32>(0.0);}
 return sample_at(uv*p.size.xy);
}
