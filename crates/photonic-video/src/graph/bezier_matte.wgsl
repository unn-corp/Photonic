// Closed cubic path flattened with an explicit 0.25 processing-pixel tolerance.
// Even-odd fill; symmetric pixel-metric feather; unassociated opaque key.
struct P { dims:vec4<f32>, options:vec4<f32>, vertices:array<vec4<f32>,512> }
@group(0) @binding(0) var<uniform> u:P;
@fragment fn fs(@builtin(position) pos:vec4<f32>)->@location(0) vec4<f32> {
    if(pos.x>=u.dims.x || pos.y>=u.dims.y){return vec4<f32>(0.0,0.0,0.0,1.0);}
    let p=pos.xy;
    let count=u32(u.options.x);
    var inside=false;var distance=1e20;
    for(var i=0u;i<count;i=i+1u){
        let a=u.vertices[i].xy;let b=u.vertices[(i+1u)%count].xy;
        let edge=b-a;let delta=p-a;let len=dot(edge,edge);
        var t=0.0;if(len>0.0){t=clamp(dot(delta,edge)/len,0.0,1.0);}
        distance=min(distance,length(delta-t*edge));
        if((a.y>p.y)!=(b.y>p.y)){
            if(p.x<a.x+(p.y-a.y)*edge.x/edge.y){inside=!inside;}
        }
    }
    let signed_distance=select(-distance,distance,inside);
    let width=max(u.options.y,0.5);
    var weight=smoothstep(-width,width,signed_distance);
    if(u.options.z>0.5){weight=1.0-weight;}
    return vec4<f32>(vec3<f32>(weight),1.0);
}
