//! Bounded AP1 scene-relative spatial scopes, independent of SDR signal scopes.
//! Positive values are log2(value / 0.18), not nits, IRE or encoded video levels.
use crate::scopes::Waveform;
pub const BINS: usize = 256;
pub const MAX_COLUMNS: usize = 256;
pub const MAX_PIXELS: usize = 16_777_216;
pub const MIN_STOP: f64 = -12.0;
pub const MAX_STOP: f64 = 12.0;
const CHANNELS: usize = 4; // AP1 Y, R, G, B
const HEADER: usize = 14; // transparent, invalid; then nonpositive/below/above per channel
const AP1_Y: [f64; 3] = [0.27222871678091454, 0.6740817658111484, 0.05368951740793705];

pub fn output_len(width: u32, height: u32) -> Result<usize, &'static str> {
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .ok_or("scene scope dimensions overflow")?;
    if width == 0 || height == 0 || pixels > MAX_PIXELS {
        return Err("scene scopes require 1..16777216 logical pixels");
    }
    Ok(HEADER + CHANNELS * BINS + CHANNELS * (width as usize).min(MAX_COLUMNS) * BINS)
}

#[derive(Clone, Debug)]
pub struct SceneScopes {
    pub histogram: [Vec<u32>; 4],
    pub waveform: [Waveform; 4],
    pub transparent: u32,
    pub invalid: u32,
    pub nonpositive: [u32; 4],
    pub below: [u32; 4],
    pub above: [u32; 4],
}
impl SceneScopes {
    pub fn from_bins(data: &[u32], width: u32, height: u32) -> Result<Self, &'static str> {
        if data.len() != output_len(width, height)? {
            return Err("incomplete scene scope bins");
        }
        let columns = (width as usize).min(MAX_COLUMNS);
        let stride = columns * BINS;
        Ok(Self {
            transparent: data[0],
            invalid: data[1],
            nonpositive: std::array::from_fn(|c| data[2 + c * 3]),
            below: std::array::from_fn(|c| data[3 + c * 3]),
            above: std::array::from_fn(|c| data[4 + c * 3]),
            histogram: std::array::from_fn(|c| {
                data[HEADER + c * BINS..HEADER + (c + 1) * BINS].to_vec()
            }),
            waveform: std::array::from_fn(|c| Waveform {
                width: columns,
                bins: BINS,
                data: data[HEADER + CHANNELS * BINS + c * stride
                    ..HEADER + CHANNELS * BINS + (c + 1) * stride]
                    .to_vec(),
            }),
        })
    }
}

/// Full logical pixels only. Each covered pixel contributes equally, regardless
/// of alpha. Out-of-range/nonpositive samples are counted, not plotted at an edge.
pub fn measure(pixels: &[[f32; 4]], width: u32, height: u32) -> Result<SceneScopes, &'static str> {
    let mut data = vec![0; output_len(width, height)?];
    if pixels.len() != width as usize * height as usize {
        return Err("incomplete scene scope pixels");
    }
    let columns = (width as usize).min(MAX_COLUMNS);
    for (i, p) in pixels.iter().enumerate() {
        if p.iter().any(|v| !v.is_finite()) || !(0.0..=1.0).contains(&p[3]) {
            data[1] += 1;
            continue;
        }
        if p[3] == 0.0 {
            data[0] += 1;
            continue;
        }
        let rgb: [f64; 3] = std::array::from_fn(|c| f64::from(p[c]) / f64::from(p[3]));
        let y = rgb.iter().zip(AP1_Y).map(|(v, k)| v * k).sum();
        for (c, v) in [y, rgb[0], rgb[1], rgb[2]].into_iter().enumerate() {
            if v <= 0.0 {
                data[2 + c * 3] += 1;
                continue;
            }
            let stop = (v / 0.18).log2();
            if stop < MIN_STOP {
                data[3 + c * 3] += 1;
                continue;
            }
            if stop >= MAX_STOP {
                data[4 + c * 3] += 1;
                continue;
            }
            let bin = (((stop - MIN_STOP) / (MAX_STOP - MIN_STOP) * BINS as f64).floor() as usize)
                .min(BINS - 1);
            let x = (i % width as usize) * columns / width as usize;
            data[HEADER + c * BINS + bin] += 1;
            data[HEADER + CHANNELS * BINS + (c * columns + x) * BINS + bin] += 1;
        }
    }
    SceneScopes::from_bins(&data, width, height)
}

/// One bounded accumulation pass; the normal scope readback queue can submit
/// this shader directly. It does not use the SDR interpretation uniform bits.
pub const SHADER: &str = r#"
@group(0) @binding(0) var t_src: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> bins: array<atomic<u32>>;
@group(0) @binding(2) var<uniform> logical: vec4<u32>;
@compute @workgroup_size(8,8,1)
fn cs_scene(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(t_src);
    if (gid.x >= min(logical.x,dims.x) || gid.y >= min(logical.y,dims.y)) { return; }
    let p = textureLoad(t_src, vec2<i32>(gid.xy), 0);
    if (any(p != p) || any(abs(p) > vec4<f32>(3.402823e38)) || p.a < 0.0 || p.a > 1.0) {
        atomicAdd(&bins[1],1u); return;
    }
    if (p.a == 0.0) { atomicAdd(&bins[0],1u); return; }
    let rgb = p.rgb/p.a;
    let values = vec4<f32>(dot(rgb,vec3<f32>(0.27222871678091454,0.6740817658111484,0.05368951740793705)),rgb);
    let columns = min(logical.x,256u);
    let x = gid.x*columns/logical.x;
    for (var c=0u; c<4u; c=c+1u) {
        let v=values[c];
        if (v <= 0.0) { atomicAdd(&bins[2u+c*3u],1u); continue; }
        let stop=log2(v/0.18);
        if (stop < -12.0) { atomicAdd(&bins[3u+c*3u],1u); continue; }
        if (stop >= 12.0) { atomicAdd(&bins[4u+c*3u],1u); continue; }
        let bin=min(255u,u32(floor((stop+12.0)/24.0*256.0)));
        atomicAdd(&bins[14u+c*256u+bin],1u);
        atomicAdd(&bins[14u+1024u+(c*columns+x)*256u+bin],1u);
    }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scene_scope_known_primary_and_power_bins() {
        // Independent fixed expectations: AP1 Y row from ACEScg primaries,
        // then floor((log2(Y / 0.18) + 12) * 256 / 24).
        for (pixel, y_bin) in [
            ([1.0, 0.0, 0.0, 1.0], 134),
            ([0.0, 1.0, 0.0, 1.0], 148),
            ([0.0, 0.0, 1.0, 1.0], 109),
        ] {
            let s = measure(&[pixel], 1, 1).unwrap();
            assert_eq!(s.histogram[0][y_bin], 1);
        }
        for (value, bin) in [
            (0.125, 122),
            (0.25, 133),
            (0.5, 143),
            (1.0, 154),
            (2.0, 165),
            (4.0, 175),
        ] {
            let s = measure(&[[value, value, value, 1.0]], 1, 1).unwrap();
            for c in 0..4 {
                assert_eq!(s.histogram[c][bin], 1);
            }
        }
    }

    #[test]
    fn scene_scope_range_alpha_and_spatial_conservation() {
        let p = [
            [0.09, 0.09, 0.09, 0.5],
            [0.0, 0.0, 0.0, 1.0],
            [-1.0, -1.0, -1.0, 1.0],
            [0.0, 0.0, 0.0, 0.0],
            [1000.0, 1000.0, 1000.0, 1.0],
            [1e-7, 1e-7, 1e-7, 1.0],
        ];
        let s = measure(&p, 6, 1).unwrap();
        assert_eq!(s.transparent, 1);
        assert_eq!(s.invalid, 0);
        for c in 0..4 {
            assert_eq!(s.nonpositive[c], 2);
            assert_eq!(s.below[c], 1);
            assert_eq!(s.above[c], 1);
            assert_eq!(s.histogram[c].iter().sum::<u32>(), 1);
            assert_eq!(s.waveform[c].data.iter().sum::<u32>(), 1);
            assert_eq!(s.waveform[c].data[..BINS].iter().sum::<u32>(), 1);
            assert_eq!(s.waveform[c].data[BINS..].iter().sum::<u32>(), 0);
        }
        assert!(output_len(4096, 4096).unwrap() * 4 < 2 * 1024 * 1024);
        assert!(output_len(0, 1).is_err());
        assert!(output_len(4097, 4096).is_err());
        assert!(measure(&p, 7, 1).is_err());
    }
    #[test]
    fn scene_scope_independent_stops_and_all_columns() {
        let mut pixels = Vec::new();
        for x in 0..513 {
            let stop = -11.7 + (x % 24) as f64;
            let v = (0.18 * 2.0f64.powf(stop)) as f32;
            pixels.push([v, v, v, 1.0]);
        }
        let s = measure(&pixels, 513, 1).unwrap();
        for c in 0..4 {
            assert_eq!(s.histogram[c].iter().sum::<u32>(), 513);
            assert_eq!(s.waveform[c].data.iter().sum::<u32>(), 513);
        }
        for (x, p) in pixels.iter().enumerate() {
            let bin = ((((f64::from(p[0]) / 0.18).log2() + 12.0) / 24.0 * 256.0).floor() as usize)
                .min(255);
            assert!(s.waveform[1].count(x * 256 / 513, bin) > 0);
        }
        let invalid = measure(&[[f32::NAN, 0.0, 0.0, 1.0]], 1, 1).unwrap();
        assert_eq!(invalid.invalid, 1);
    }
}
