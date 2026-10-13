// Static normalized polygon, even-odd fill; inward local-space edge feather.
struct P { center:vec4<f32>, aux:vec4<f32>, dims:vec4<f32>, vertices:array<vec4<f32>,128> }
@group(0) @binding(0) var<uniform> u:P;
@fragment fn fs(@builtin(position) pos:vec4<f32>)->@location(0) vec4<f32> {
    if(pos.x>=u.dims.x || pos.y>=u.dims.y || u.center.z<=1e-6 || u.center.w<=1e-6){return vec4<f32>(0.0);}
    let d=pos.xy/u.dims.xy-u.center.xy;
    let p=vec2<f32>(d.x*u.aux.x-d.y*u.aux.y,d.x*u.aux.y+d.y*u.aux.x)/u.center.zw;
    let count=u32(u.aux.w);
    var inside=false; var distance=1e20;
    for(var i=0u;i<count;i=i+1u){
        let a=u.vertices[i].xy; let b=u.vertices[(i+1u)%count].xy;
        let edge=b-a; let delta=p-a; let len=dot(edge,edge);
        var t=0.0; if(len>0.0){t=clamp(dot(delta,edge)/len,0.0,1.0);}
        distance=min(distance,length(delta-t*edge));
        if((a.y>p.y)!=(b.y>p.y)){
            if(p.x<a.x+(p.y-a.y)*edge.x/edge.y){inside=!inside;}
        }
    }
    var alpha=0.0;
    if(inside){alpha=1.0;if(u.aux.z>0.0){alpha=smoothstep(0.0,u.aux.z,distance);}}
    return vec4<f32>(alpha);
}
