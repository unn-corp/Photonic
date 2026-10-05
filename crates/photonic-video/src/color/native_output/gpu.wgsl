// SPDX-License-Identifier: Apache-2.0
// Copyright Contributors to the ACES Project.
// Isolated ACES 2-style SDR output parity shader. Row layout is supplied by
// Aces2SdrOutput::gpu_rows; no managed preview/export path selects it yet.

@group(0) @binding(0) var source: texture_2d<f32>;
struct Params { rows: array<vec4<f32>> }
@group(0) @binding(1) var<storage, read> params: Params;

struct VertexOut { @builtin(position) position: vec4<f32> }

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> VertexOut {
    var positions = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0)
    );
    var result: VertexOut;
    result.position = vec4<f32>(positions[index], 0.0, 1.0);
    return result;
}

fn matrix_rows(first: u32, value: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        dot(params.rows[first].xyz, value),
        dot(params.rows[first + 1u].xyz, value),
        dot(params.rows[first + 2u].xyz, value)
    );
}

fn compress_cone(value: f32) -> f32 {
    let magnitude = pow(abs(value), 0.42);
    return sign(value) * magnitude / (27.13 + magnitude);
}

fn expand_cone(value: f32) -> f32 {
    let magnitude = min(abs(value), 0.99);
    let response = 27.13 * magnitude / (1.0 - magnitude);
    return sign(value) * pow(response, 1.0 / 0.42);
}

fn source_jmh(ap0: vec3<f32>) -> vec3<f32> {
    let cone = matrix_rows(3u, ap0);
    let aab = matrix_rows(6u, vec3<f32>(
        compress_cone(cone.x), compress_cone(cone.y), compress_cone(cone.z)
    ));
    if (aab.x <= 0.0) { return vec3<f32>(0.0); }
    var hue = degrees(atan2(aab.z, aab.y));
    if (hue < 0.0) { hue += 360.0; }
    return vec3<f32>(
        100.0 * pow(aab.x, params.rows[16u].y),
        length(aab.yz),
        hue
    );
}

fn limit_rgb(jmh: vec3<f32>) -> vec3<f32> {
    let angle = radians(jmh.z);
    let aab = vec3<f32>(
        pow(jmh.x / 100.0, 1.0 / params.rows[17u].x),
        jmh.y * cos(angle),
        jmh.y * sin(angle)
    );
    let cone = matrix_rows(9u, aab);
    return matrix_rows(12u, vec3<f32>(
        expand_cone(cone.x), expand_cone(cone.y), expand_cone(cone.z)
    ));
}

fn tone_j(lightness: f32) -> f32 {
    let source_model = params.rows[16u];
    let achromatic = pow(lightness / 100.0, 1.0 / source_model.y);
    let luminance = expand_cone(source_model.z * achromatic) / source_model.w;
    let scene = max(luminance / 100.0, 0.0);
    let tone = params.rows[15u];
    let michaelis = tone.x * pow(scene / (scene + tone.y), tone.z);
    let nits = michaelis * michaelis / (michaelis + tone.w) * 100.0;
    let response = compress_cone(nits * source_model.w);
    return 100.0 * pow(response / source_model.z, source_model.y);
}

fn toe(value: f32, limit: f32, first_in: f32, second_in: f32) -> f32 {
    if (value > limit) { return value; }
    let second = max(second_in, 0.001);
    let first = sqrt(first_in * first_in + second * second);
    let ratio = (limit + first) / (limit + second);
    let minus_b = ratio * value - first;
    let minus_c = second * ratio * value;
    return 0.5 * (minus_b + sqrt(minus_b * minus_b + 4.0 * minus_c));
}

fn chroma_norm(hue: f32) -> f32 {
    let angle = radians(hue);
    let a = cos(angle);
    let b = sin(angle);
    let cos2 = a * a - b * b;
    let sin2 = 2.0 * a * b;
    let cos3 = 4.0 * a * a * a - 3.0 * a;
    let sin3 = 3.0 * b - 4.0 * b * b * b;
    return (11.34072 * a + 16.46899 * cos2 + 7.88380 * cos3
        + 14.66441 * b - 6.37224 * sin2 + 9.19364 * sin3 + 77.12896)
        * params.rows[17u].w;
}

fn shape_chroma(input: vec3<f32>) -> vec3<f32> {
    let mapped_j = tone_j(input.x);
    if (input.y <= 0.0) { return vec3<f32>(mapped_j, 0.0, input.z); }
    if (input.x <= 0.0 || mapped_j <= 0.0) { return vec3<f32>(0.0, 0.0, input.z); }
    let max_j = params.rows[17u].y;
    let normalized_j = mapped_j / max_j;
    let shadow = max(1.0 - normalized_j, 0.0);
    let norm = chroma_norm(input.z);
    let hue = input.z - floor(input.z / 360.0) * 360.0;
    let index = u32(floor(hue));
    let fraction = hue - f32(index);
    let reach = mix(params.rows[21u + index].x, params.rows[22u + index].x, fraction);
    let limit = pow(normalized_j, params.rows[17u].z) * reach / norm;
    var m = input.y * pow(mapped_j / input.x, params.rows[17u].z) / norm;
    let chroma = params.rows[18u];
    m = limit - toe(limit - m, limit - 0.001,
        shadow * chroma.x, sqrt(normalized_j * normalized_j + chroma.y));
    m = toe(m, limit, normalized_j * chroma.z, shadow) * norm;
    return vec3<f32>(mapped_j, m, input.z);
}

fn hue_params(hue_degrees: f32) -> vec4<f32> {
    // The matching reach-at-Jmax value is fetched separately from row 1 of
    // each entry. This function returns cusp J/M and upper-hull gamma.
    let hue = hue_degrees - floor(hue_degrees / 360.0) * 360.0;
    let count = u32(params.rows[19u].x);
    var low = 0u;
    var high = count - 1u;
    while (low + 1u < high) {
        let middle = (low + high) / 2u;
        if (params.rows[382u + middle * 2u].x <= hue) {
            low = middle;
        } else {
            high = middle;
        }
    }
    let a = params.rows[382u + low * 2u];
    let b = params.rows[382u + (low + 1u) * 2u];
    let fraction = (hue - a.x) / (b.x - a.x);
    let reach_a = params.rows[383u + low * 2u].x;
    let reach_b = params.rows[383u + (low + 1u) * 2u].x;
    return vec4<f32>(
        mix(a.y, b.y, fraction),
        mix(a.z, b.z, fraction),
        mix(a.w, b.w, fraction),
        mix(reach_a, reach_b, fraction)
    );
}

fn axis_intersection(j: f32, m: f32, focus: f32, gain: f32, max_j: f32) -> f32 {
    if (m == 0.0) { return j; }
    let scaled = m / gain;
    let a = scaled / focus;
    if (j < focus) {
        let b = 1.0 - scaled;
        let c = -j;
        return -2.0 * c / (b + sqrt(b * b - 4.0 * a * c));
    }
    let b = -(1.0 + scaled + max_j * a);
    let c = max_j * scaled + j;
    return -2.0 * c / (b - sqrt(b * b - 4.0 * a * c));
}

fn estimate_boundary(axis_j: f32, slope: f32, inv_gamma: f32,
    max_j: f32, max_m: f32, reference_j: f32) -> f32 {
    let shifted = reference_j * pow(axis_j / reference_j, inv_gamma);
    return shifted * max_m / (max_j - slope * max_m);
}

fn focus_gain(j: f32, threshold: f32, max_j: f32) -> f32 {
    var gain = max_j * 1.35;
    if (j > threshold) {
        let adjustment = log((max_j - threshold) / max(0.0001, max_j - j)) / log(10.0);
        gain *= adjustment * adjustment + 1.0;
    }
    return gain;
}

fn gamut_map(input: vec3<f32>) -> vec3<f32> {
    let max_j = params.rows[17u].y;
    if (input.x <= 0.0) { return vec3<f32>(0.0, 0.0, input.z); }
    if (input.y <= 0.0) { return input; }
    if (input.x > max_j) { return vec3<f32>(input.x, 0.0, input.z); }
    let hue = hue_params(input.z);
    let cusp_j = hue.x;
    let cusp_m = hue.y;
    let mid_j = params.rows[18u].w;
    let focus_blend = min(1.0, 1.3 - cusp_j / max_j);
    let focus_j = mix(cusp_j, mid_j, focus_blend);
    let threshold_j = mix(cusp_j, max_j, 0.3);
    let gain = focus_gain(input.x, threshold_j, max_j);
    let axis_j = axis_intersection(input.x, input.y, focus_j, gain, max_j);
    let direction = select(max_j - axis_j, axis_j, axis_j < focus_j);
    let slope = direction * (axis_j - focus_j) / (focus_j * gain);
    let axis_cusp = axis_intersection(cusp_j, cusp_m, focus_j, gain, max_j);
    let lower = estimate_boundary(axis_j, slope, 1.0 / 1.14,
        cusp_j, cusp_m, axis_cusp);
    let upper = estimate_boundary(max_j - axis_j, -slope, hue.z,
        max_j - cusp_j, cusp_m, max_j - axis_cusp);
    let smoothing = 0.12 * cusp_m;
    let overlap = max(smoothing - abs(lower - upper), 0.0) / smoothing;
    let limit_m = min(lower, upper) - overlap * overlap * overlap * smoothing / 6.0;
    let model_gamma = 0.59 * (1.48 + sqrt(0.2));
    let reach_m = estimate_boundary(axis_j, slope, 1.0 / model_gamma,
        max_j, hue.w, max_j);
    let proportion = max(limit_m / reach_m, 0.75);
    let threshold_m = proportion * limit_m;
    var remapped_m = input.y;
    if (input.y > threshold_m && proportion < 1.0) {
        let gamut_room = limit_m - threshold_m;
        let reach_room = reach_m - threshold_m;
        let scale = reach_room / (reach_room / gamut_room - 1.0);
        let n = (input.y - threshold_m) / scale;
        remapped_m = threshold_m + scale * n / (1.0 + n);
    }
    let safe_m = min(remapped_m, limit_m);
    return vec3<f32>(axis_j + safe_m * slope, safe_m, input.z);
}

fn encode(value: f32) -> f32 {
    let bounded = clamp(value, 0.0, 1.0);
    if (bounded <= 0.0031308) { return 12.92 * bounded; }
    return 1.055 * pow(bounded, 1.0 / 2.4) - 0.055;
}

fn encode_video(value: f32) -> f32 {
    let bounded = clamp(value, 0.0, 1.0);
    if (bounded <= 0.018) { return 4.5 * bounded; }
    return 1.099 * pow(bounded, 0.45) - 0.099;
}

fn render_sdr(input: VertexOut, video: bool) -> vec4<f32> {
    let pixel = textureLoad(source, vec2<i32>(input.position.xy), 0);
    if (pixel.a <= 0.0) { return vec4<f32>(0.0); }
    let straight = pixel.rgb / pixel.a;
    let clamped_ap1 = clamp(straight, vec3<f32>(0.0), vec3<f32>(params.rows[16u].x));
    let ap0 = matrix_rows(0u, clamped_ap1);
    let appearance = source_jmh(ap0);
    if (appearance.x <= 0.0) { return vec4<f32>(0.0, 0.0, 0.0, pixel.a); }
    let shaped = shape_chroma(appearance);
    let mapped = gamut_map(shaped);
    let linear = limit_rgb(mapped);
    if (video) {
        return vec4<f32>(
            vec3<f32>(encode_video(linear.r), encode_video(linear.g), encode_video(linear.b)) * pixel.a,
            pixel.a
        );
    }
    return vec4<f32>(
        vec3<f32>(encode(linear.r), encode(linear.g), encode(linear.b)) * pixel.a,
        pixel.a
    );
}

@fragment
fn fragment(input: VertexOut) -> @location(0) vec4<f32> {
    return render_sdr(input, false);
}

@fragment
fn video_fragment(input: VertexOut) -> @location(0) vec4<f32> {
    return render_sdr(input, true);
}
