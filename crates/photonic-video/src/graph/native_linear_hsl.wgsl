@group(0) @binding(0) var source: texture_2d<f32>;
struct Params {
    info: vec4<f32>, // kind, identity, logical width, height
    a0: vec4<f32>, a1: vec4<f32>, a2: vec4<f32>, // AP1 to linear Rec.709
    b0: vec4<f32>, b1: vec4<f32>, b2: vec4<f32>, // inverse
    controls: vec4<f32>,
}
@group(0) @binding(1) var<uniform> p: Params;
fn wrap(v: f32, period: f32) -> f32 { return v-floor(v/period)*period; }
fn rgb_to_hsl(rgb: vec3<f32>) -> vec3<f32> {
    let hi=max(rgb.r,max(rgb.g,rgb.b));
    let lo=min(rgb.r,min(rgb.g,rgb.b));
    let l=(hi+lo)*0.5;
    let d=hi-lo;
    if(abs(d)<1e-9){return vec3<f32>(0.0,0.0,clamp(l,0.0,1.0));}
    let s=d/max(1.0-abs(2.0*l-1.0),1e-9);
    var h=0.0;
    if(hi==rgb.r){h=60.0*wrap((rgb.g-rgb.b)/d,6.0);}
    else if(hi==rgb.g){h=60.0*((rgb.b-rgb.r)/d+2.0);}
    else{h=60.0*((rgb.r-rgb.g)/d+4.0);}
    return vec3<f32>(wrap(h,360.0),clamp(s,0.0,1.0),clamp(l,0.0,1.0));
}
fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    let h=wrap(hsl.x,360.0)/60.0;
    let l=clamp(hsl.z,0.0,1.0);
    let c=(1.0-abs(2.0*l-1.0))*clamp(hsl.y,0.0,1.0);
    let x=c*(1.0-abs(wrap(h,2.0)-1.0));
    var rgb=vec3<f32>(c,0.0,x);
    if(h<1.0){rgb=vec3<f32>(c,x,0.0);}
    else if(h<2.0){rgb=vec3<f32>(x,c,0.0);}
    else if(h<3.0){rgb=vec3<f32>(0.0,c,x);}
    else if(h<4.0){rgb=vec3<f32>(0.0,x,c);}
    else if(h<5.0){rgb=vec3<f32>(x,0.0,c);}
    return rgb+vec3<f32>(l-c*0.5);
}
fn extended_hsl(rgb: vec3<f32>) -> vec3<f32> {
    let lo=min(rgb.r,min(rgb.g,rgb.b));
    let hi=max(rgb.r,max(rgb.g,rgb.b));
    let vibrance=p.info.x>0.5;
    let relative_chroma=(hi-lo)/max(1.0,max(abs(lo),abs(hi)));
    if(vibrance && relative_chroma<=1e-6){return rgb;}
    let low=2.0*min(lo,0.0);
    let high=1.0+2.0*max(hi-1.0,0.0);
    var hsl=rgb_to_hsl((rgb-vec3<f32>(low))/(high-low));
    var lightness=0.0;
    if(vibrance){
        let t=clamp((relative_chroma-1e-6)/(1e-4-1e-6),0.0,1.0);
        let gate=t*t*(3.0-2.0*t);
        hsl.y=clamp(hsl.y+p.controls.x*(1.0-hsl.y)*gate,0.0,1.0);
    }
    else{
        hsl.x=wrap(hsl.x+p.controls.x,360.0);
        hsl.y=clamp(hsl.y*(1.0+p.controls.y),0.0,1.0);
        lightness=p.controls.z;
    }
    let t=abs(lightness);
    let keep=1.0-t;
    let physical_l=keep*(hi+lo)*0.5+t*select(0.0,1.0,lightness>=0.0);
    if(hsl.y==0.0){return vec3<f32>(physical_l);}
    let new_low=keep*low;
    let new_high=1.0+keep*(high-1.0);
    hsl.z=(physical_l-new_low)/(new_high-new_low);
    return vec3<f32>(new_low)+hsl_to_rgb(hsl)*(new_high-new_low);
}
@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    if(pos.x>=p.info.z||pos.y>=p.info.w){return vec4<f32>(0.0);}
    let src=textureLoad(source,vec2<i32>(pos.xy),0);
    if(src.a<=0.0){return vec4<f32>(0.0);}
    if(p.info.y>0.5){return src;}
    let straight=src.rgb/src.a;
    let linear=vec3<f32>(dot(p.a0.xyz,straight),dot(p.a1.xyz,straight),dot(p.a2.xyz,straight));
    let min_rgb=min(linear.r,min(linear.g,linear.b));
    let max_rgb=max(linear.r,max(linear.g,linear.b));
    if(p.info.x>0.5 && max_rgb-min_rgb<=1e-6*max(1.0,max(abs(min_rgb),abs(max_rgb)))){return src;}
    // No transfer curve: these catalog entries explicitly declare LinearStraight.
    let rgb=extended_hsl(linear);
    let ap1=vec3<f32>(dot(p.b0.xyz,rgb),dot(p.b1.xyz,rgb),dot(p.b2.xyz,rgb));
    return vec4<f32>(clamp(ap1*src.a,vec3<f32>(-65504.0),vec3<f32>(65504.0)),src.a);
}
