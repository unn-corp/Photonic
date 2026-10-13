@group(0) @binding(0) var source:texture_2d<f32>;
struct Params {
    info:vec4<f32>, // radius, threshold, logical width, height
    scale:vec4<f32>,
    m0:vec4<f32>, m1:vec4<f32>, m2:vec4<f32>,
}
@group(0) @binding(1) var<uniform> p:Params;
fn sample_at(point:vec2<f32>)->vec4<f32>{
    let size=vec2<i32>(p.info.zw);
    let q=clamp(point,vec2<f32>(0.0),vec2<f32>(size-vec2<i32>(1)));
    let base=vec2<i32>(floor(q));let f=fract(q);
    let a=textureLoad(source,base,0);
    if(f.x==0.0&&f.y==0.0){return a;}
    let hi=min(base+vec2<i32>(1),size-vec2<i32>(1));
    if(f.y==0.0){return mix(a,textureLoad(source,vec2<i32>(hi.x,base.y),0),f.x);}
    if(f.x==0.0){return mix(a,textureLoad(source,vec2<i32>(base.x,hi.y),0),f.y);}
    let b=textureLoad(source,vec2<i32>(hi.x,base.y),0);
    let c=textureLoad(source,vec2<i32>(base.x,hi.y),0);
    let d=textureLoad(source,hi,0);
    return mix(mix(a,b,f.x),mix(c,d,f.x),f.y);
}
@fragment
fn fs(@builtin(position) pos:vec4<f32>)->@location(0) vec4<f32>{
    if(pos.x>=p.info.z||pos.y>=p.info.w){return vec4<f32>(0.0);}
    let xy=vec2<i32>(pos.xy);let center=textureLoad(source,xy,0);
    if(p.info.x==0.0||p.info.y==0.0){return center;}
    if(center.a<=0.0){return vec4<f32>(0.0);}
    let radius=i32(p.info.x);let sigma_s=max(p.info.x,1.0);
    let sigma_r=max(p.info.y,1.0/255.0);
    let inv_s=0.5/(sigma_s*sigma_s);let inv_r=0.5/(sigma_r*sigma_r);
    var sum=vec4<f32>(0.0);
    for(var dy=-radius;dy<=radius;dy++){
        for(var dx=-radius;dx<=radius;dx++){
            let v=sample_at(vec2<f32>(xy)+vec2<f32>(f32(dx),f32(dy))*p.scale.xy);
            let difference=v.rgb-center.rgb;
            let delta=vec3<f32>(dot(p.m0.xyz,difference),dot(p.m1.xyz,difference),dot(p.m2.xyz,difference));
            let weight=exp(-f32(dx*dx+dy*dy)*inv_s)*exp(-dot(delta,delta)*inv_r);
            sum+=v*weight;
        }
    }
    if(sum.a<=0.0){return center;}
    return vec4<f32>(clamp(sum.rgb*(center.a/sum.a),vec3<f32>(-65504.0),vec3<f32>(65504.0)),center.a);
}
