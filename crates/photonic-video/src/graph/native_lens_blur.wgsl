@group(0) @binding(0) var source:texture_2d<f32>;
struct Params { info:vec4<f32>, scale:vec4<f32> }
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
    if(p.info.x<1.0){return center;}
    let bound=i32(ceil(p.info.x));let radius2=p.info.x*p.info.x;
    var sum=vec4<f32>(0.0);var correction=vec4<f32>(0.0);var count=0u;
    for(var dy=-bound;dy<=bound;dy++){
        for(var dx=-bound;dx<=bound;dx++){
            if(f32(dx*dx+dy*dy)>radius2){continue;}
            let v=sample_at(vec2<f32>(xy)+vec2<f32>(f32(dx),f32(dy))*p.scale.xy);
            let corrected=v-correction;let next=sum+corrected;
            correction=(next-sum)-corrected;sum=next;count++;
        }
    }
    let average=sum/f32(count);
    return vec4<f32>(clamp(average.rgb,vec3<f32>(-65504.0),vec3<f32>(65504.0)),average.a);
}
