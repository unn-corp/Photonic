//! Bounded translation-only window tracking, producing proposals rather than edits.
//! Mean-subtracted normalized correlation: J. P. Lewis, section 2,
//! https://scribblethink.org/Work/nvisionInterface/nip.html . This is a direct
//! spatial implementation, not the paper's FFT acceleration. Scale, rotation,
//! perspective and deformation are deliberately outside this contract.
use serde::{Deserialize, Serialize};

/// Explicit opt-in: short-term templates can drift despite consistency checks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackingMethod {
    #[default]
    FixedReference,
    AdaptiveBidirectional,
}

pub const MAX_INPUT_PIXELS: usize = 16_777_216;
pub const MAX_ANALYSIS_SIDE: usize = 256;
pub const MAX_SEARCH_RADIUS: i32 = 24;

#[derive(Clone, Debug)]
pub struct TrackingPlane {
    width: usize,
    height: usize,
    values: Vec<Option<f64>>,
}
impl TrackingPlane {
    /// Bounded area-filtered sampling of straight working RGB. The average of
    /// channels is a texture signal, not a physical luminance measurement.
    /// Exact input dimensions/quality remain the caller's provenance contract.
    pub fn from_rgba(pixels: &[[f32; 4]], width: u32, height: u32) -> Result<Self, &'static str> {
        let count = (width as usize)
            .checked_mul(height as usize)
            .ok_or("image size overflow")?;
        if width == 0 || height == 0 || count > MAX_INPUT_PIXELS || count != pixels.len() {
            return Err("tracking input is incomplete or exceeds 16,777,216 pixels");
        }
        if pixels
            .iter()
            .any(|p| p.iter().any(|c| !c.is_finite()) || !(0.0..=1.0).contains(&p[3]))
        {
            return Err("tracking input has non-finite values or invalid alpha");
        }
        let scale = (MAX_ANALYSIS_SIDE as f64 / f64::from(width.max(height))).min(1.0);
        let w = (f64::from(width) * scale).round().max(1.0) as usize;
        let h = (f64::from(height) * scale).round().max(1.0) as usize;
        let mut values = Vec::with_capacity(w * h);
        for y in 0..h {
            let y0 = y as f64 * f64::from(height) / h as f64;
            let y1 = (y + 1) as f64 * f64::from(height) / h as f64;
            for x in 0..w {
                let x0 = x as f64 * f64::from(width) / w as f64;
                let x1 = (x + 1) as f64 * f64::from(width) / w as f64;
                let mut color = 0.;
                let mut alpha = 0.;
                // Exact pixel-area overlap avoids high-frequency nearest-sample
                // aliasing. Accumulate premultiplied color and coverage before
                // unpremultiplication; transparent hidden RGB contributes zero.
                for sy in y0.floor() as usize..(y1.ceil() as usize).min(height as usize) {
                    let wy = (y1.min((sy + 1) as f64) - y0.max(sy as f64)).max(0.);
                    for sx in x0.floor() as usize..(x1.ceil() as usize).min(width as usize) {
                        let wx = (x1.min((sx + 1) as f64) - x0.max(sx as f64)).max(0.);
                        let weight = wx * wy;
                        let p = pixels[sy * width as usize + sx];
                        if p[3] > 0. {
                            alpha += weight * f64::from(p[3]);
                            color +=
                                weight * (f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2])) / 3.;
                        }
                    }
                }
                let area = (x1 - x0) * (y1 - y0);
                values.push((alpha / area > 0.01).then(|| color / alpha));
            }
        }
        Ok(Self {
            width: w,
            height: h,
            values,
        })
    }
    pub fn size(&self) -> [usize; 2] {
        [self.width, self.height]
    }
    fn patch(&self, x: i32, y: i32, half: [i32; 2]) -> Option<Vec<f64>> {
        if x - half[0] < 0
            || y - half[1] < 0
            || x + half[0] >= self.width as i32
            || y + half[1] >= self.height as i32
        {
            return None;
        }
        let mut p = Vec::with_capacity(((2 * half[0] + 1) * (2 * half[1] + 1)) as usize);
        for yy in y - half[1]..=y + half[1] {
            for xx in x - half[0]..=x + half[0] {
                p.push(self.values[yy as usize * self.width + xx as usize]?);
            }
        }
        Some(p)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TrackingConfig {
    pub search_radius: i32,
    pub minimum_score: f64,
    pub minimum_margin: f64,
}
impl Default for TrackingConfig {
    fn default() -> Self {
        Self {
            search_radius: 12,
            minimum_score: 0.75,
            minimum_margin: 0.05,
        }
    }
}
impl TrackingConfig {
    pub fn validate(self) -> Result<(), &'static str> {
        if !(2..=MAX_SEARCH_RADIUS).contains(&self.search_radius)
            || !self.minimum_score.is_finite()
            || !(0.5..=1.0).contains(&self.minimum_score)
            || !self.minimum_margin.is_finite()
            || !(0.0..=0.5).contains(&self.minimum_margin)
        {
            return Err("invalid bounded tracking configuration");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TrackingStop {
    Cancelled,
    DimensionsChanged,
    OutOfBounds,
    LowTexture,
    LowConfidence,
    Ambiguous,
    SearchLimit,
    Inconsistent,
}

#[derive(Clone, Debug, Serialize)]
pub struct TrackingMatch {
    pub center: [f64; 2],
    pub score: f64,
    pub margin: f64,
}

pub struct TranslationTracker {
    size: [usize; 2],
    half: [i32; 2],
    template: Vec<f64>,
    template_energy: f64,
    origin: [i32; 2],
    original_center: [f64; 2],
    current: [f64; 2],
    config: TrackingConfig,
}

fn centered(mut values: Vec<f64>) -> Option<(Vec<f64>, f64)> {
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let mut energy = 0.0;
    for value in &mut values {
        *value -= mean;
        energy += *value * *value;
    }
    (energy.is_finite() && energy / values.len() as f64 > 1e-12).then_some((values, energy))
}
fn correlation(a: &[f64], ae: f64, b: Vec<f64>) -> Option<f64> {
    let (b, be) = centered(b)?;
    let score = a.iter().zip(b).map(|(a, b)| a * b).sum::<f64>() / (ae * be).sqrt();
    score.is_finite().then_some(score.clamp(-1.0, 1.0))
}
fn subpixel(left: Option<f64>, center: f64, right: Option<f64>) -> f64 {
    let (Some(left), Some(right)) = (left, right) else {
        return 0.0;
    };
    let denominator = left - 2.0 * center + right;
    if denominator >= -1e-9 {
        0.0
    } else {
        (0.5 * (left - right) / denominator).clamp(-0.5, 0.5)
    }
}
impl TranslationTracker {
    /// Center and half-size are normalized to the post-transform clip canvas.
    /// A center patch of 9..33 pixels per dimension is used, bounded by the
    /// requested primitive window. No rotation or scale is inferred.
    pub fn new(
        frame: &TrackingPlane,
        center: [f64; 2],
        half_size: [f64; 2],
        config: TrackingConfig,
    ) -> Result<Self, TrackingStop> {
        if config.validate().is_err()
            || center
                .iter()
                .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
            || half_size
                .iter()
                .any(|v| !v.is_finite() || *v <= 0.0 || *v > 1.0)
        {
            return Err(TrackingStop::OutOfBounds);
        }
        let size = frame.size();
        let origin = [
            (center[0] * size[0] as f64 - 0.5).round() as i32,
            (center[1] * size[1] as f64 - 0.5).round() as i32,
        ];
        let half = [
            (half_size[0] * size[0] as f64).floor() as i32,
            (half_size[1] * size[1] as f64).floor() as i32,
        ];
        if half.iter().any(|v| *v < 4) {
            return Err(TrackingStop::LowTexture);
        }
        let half = [half[0].min(16), half[1].min(16)];
        let p = frame
            .patch(origin[0], origin[1], half)
            .ok_or(TrackingStop::OutOfBounds)?;
        let (template, template_energy) = centered(p).ok_or(TrackingStop::LowTexture)?;
        Ok(Self {
            size,
            half,
            template,
            template_energy,
            origin,
            original_center: center,
            current: origin.map(f64::from),
            config,
        })
    }
    pub fn step(
        &mut self,
        frame: &TrackingPlane,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<TrackingMatch, TrackingStop> {
        if cancelled() {
            return Err(TrackingStop::Cancelled);
        }
        if frame.size() != self.size {
            return Err(TrackingStop::DimensionsChanged);
        }
        let at = self.current.map(|v| v.round() as i32);
        let radius = self.config.search_radius;
        let mut scores = Vec::with_capacity(((2 * radius + 1) * (2 * radius + 1)) as usize);
        for dy in -radius..=radius {
            if cancelled() {
                return Err(TrackingStop::Cancelled);
            }
            for dx in -radius..=radius {
                if let Some(p) = frame.patch(at[0] + dx, at[1] + dy, self.half) {
                    if let Some(score) = correlation(&self.template, self.template_energy, p) {
                        scores.push((dx, dy, score));
                    }
                }
            }
        }
        scores.sort_by(|a, b| {
            b.2.total_cmp(&a.2)
                .then_with(|| (a.0 * a.0 + a.1 * a.1).cmp(&(b.0 * b.0 + b.1 * b.1)))
                .then(a.1.cmp(&b.1))
                .then(a.0.cmp(&b.0))
        });
        let &(dx, dy, score) = scores.first().ok_or(TrackingStop::LowTexture)?;
        if score < self.config.minimum_score {
            return Err(TrackingStop::LowConfidence);
        }
        let runner = scores
            .iter()
            .find(|(x, y, _)| (x - dx).abs().max((y - dy).abs()) >= 3)
            .map(|v| v.2)
            .unwrap_or(-1.0);
        let margin = score - runner;
        if margin < self.config.minimum_margin {
            return Err(TrackingStop::Ambiguous);
        }
        if dx.abs() == radius || dy.abs() == radius {
            return Err(TrackingStop::SearchLimit);
        }
        let get = |x, y| {
            scores
                .iter()
                .find(|(xx, yy, _)| *xx == x && *yy == y)
                .map(|v| v.2)
        };
        let pos = [
            f64::from(at[0] + dx) + subpixel(get(dx - 1, dy), score, get(dx + 1, dy)),
            f64::from(at[1] + dy) + subpixel(get(dx, dy - 1), score, get(dx, dy + 1)),
        ];
        let center = [
            self.original_center[0] + (pos[0] - f64::from(self.origin[0])) / self.size[0] as f64,
            self.original_center[1] + (pos[1] - f64::from(self.origin[1])) / self.size[1] as f64,
        ];
        if center.iter().any(|v| !(0.0..=1.0).contains(v)) {
            return Err(TrackingStop::OutOfBounds);
        }
        self.current = pos;
        Ok(TrackingMatch {
            center,
            score,
            margin,
        })
    }
}

/// Optional short-term translation proposal with a forward/backward check.
/// Motivated by Kalal et al. (ICPR 2010) consistency checking, not an
/// implementation or accuracy claim for their Median Flow tracker:
/// https://cmp.felk.cvut.cz/ftp/articles/matas/kalal-2010-fb_track-icpr.pdf .
/// Confidence can still fail on similar distractors, background or deformation.
#[derive(Clone, Debug, Serialize)]
pub struct BidirectionalTrackingMatch {
    pub center: [f64; 2],
    pub score: f64,
    pub margin: f64,
    pub backward_score: f64,
    pub backward_margin: f64,
    pub roundtrip_error_pixels: f64,
}

pub struct BidirectionalTranslationTracker {
    previous: TrackingPlane,
    center: [f64; 2],
    initial_center: [f64; 2],
    origin: [f64; 2],
    position: [f64; 2],
    half_size: [f64; 2],
    config: TrackingConfig,
}

impl BidirectionalTranslationTracker {
    pub fn new(
        frame: &TrackingPlane,
        center: [f64; 2],
        half_size: [f64; 2],
        config: TrackingConfig,
    ) -> Result<Self, TrackingStop> {
        let initial = TranslationTracker::new(frame, center, half_size, config)?;
        let origin = initial.origin.map(f64::from);
        Ok(Self {
            previous: frame.clone(),
            center,
            initial_center: center,
            origin,
            position: origin,
            half_size,
            config,
        })
    }

    /// Each direction keeps the same NCC, ambiguity and search-edge guards.
    /// Only a successful round trip commits the template/frame update. One
    /// prior bounded analysis plane is retained; no unbounded template history.
    pub fn step(
        &mut self,
        frame: &TrackingPlane,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<BidirectionalTrackingMatch, TrackingStop> {
        if cancelled() {
            return Err(TrackingStop::Cancelled);
        }
        if frame.size() != self.previous.size() {
            return Err(TrackingStop::DimensionsChanged);
        }
        let size = frame.size();
        // Templates are sampled at integer pixel centers. Keep their analysis
        // coordinates separate from the original user/window center offset;
        // otherwise repeated subpixel offsets accumulate drift on a static image.
        let pixel_center = |p: [f64; 2]| {
            [
                (p[0].round() + 0.5) / size[0] as f64,
                (p[1].round() + 0.5) / size[1] as f64,
            ]
        };
        let mut forward = TranslationTracker::new(
            &self.previous,
            pixel_center(self.position),
            self.half_size,
            self.config,
        )?;
        let candidate = forward.step(frame, &mut cancelled)?;
        let position = forward.current;
        let mut reverse =
            TranslationTracker::new(frame, pixel_center(position), self.half_size, self.config)?;
        let recovered = reverse.step(&self.previous, &mut cancelled)?;
        let error =
            (reverse.current[0] - self.position[0]).hypot(reverse.current[1] - self.position[1]);
        let center = [
            self.initial_center[0] + (position[0] - self.origin[0]) / size[0] as f64,
            self.initial_center[1] + (position[1] - self.origin[1]) / size[1] as f64,
        ];
        if center.iter().any(|v| !(0.0..=1.0).contains(v)) {
            return Err(TrackingStop::OutOfBounds);
        }
        if !error.is_finite() || error > 1.0 {
            return Err(TrackingStop::Inconsistent);
        }
        if cancelled() {
            return Err(TrackingStop::Cancelled);
        }
        self.previous = frame.clone();
        self.center = center;
        self.position = position;
        Ok(BidirectionalTrackingMatch {
            center,
            score: candidate.score,
            margin: candidate.margin,
            backward_score: recovered.score,
            backward_margin: recovered.margin,
            roundtrip_error_pixels: error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn texture(dx: i32, dy: i32, gain: f64, offset: f64) -> TrackingPlane {
        let mut pixels = vec![[0.0; 4]; 96 * 72];
        for y in 0..72i32 {
            for x in 0..96i32 {
                let xx = x - dx;
                let yy = y - dy;
                let v = if (24..72).contains(&xx) && (16..56).contains(&yy) {
                    let h =
                        (xx as u32).wrapping_mul(0x9e3779b9) ^ (yy as u32).wrapping_mul(0x85ebca6b);
                    ((h ^ (h >> 13)).wrapping_mul(0xc2b2ae35) >> 8) as f64 / 16_777_216.0
                } else {
                    0.0
                };
                let v = (v * gain + offset) as f32;
                pixels[y as usize * 96 + x as usize] = [v * 0.5, v * 0.5, v * 0.5, 0.5];
            }
        }
        TrackingPlane::from_rgba(&pixels, 96, 72).unwrap()
    }
    fn tracker() -> TranslationTracker {
        TranslationTracker::new(
            &texture(0, 0, 1.0, 0.0),
            [0.5, 0.5],
            [0.13, 0.15],
            TrackingConfig::default(),
        )
        .unwrap()
    }
    #[test]
    fn forward_backward_translation_and_positive_photometric_change() {
        for direction in [-1, 1] {
            let mut t = tracker();
            for i in 1..=5 {
                let x = i * direction;
                let y = -i * direction;
                let found = t.step(&texture(x, y, 2.0, -1.0), || false).unwrap();
                assert!((found.center[0] - (0.5 + x as f64 / 96.0)).abs() < 0.002);
                assert!((found.center[1] - (0.5 + y as f64 / 72.0)).abs() < 0.002);
                assert!(found.score > 0.999);
            }
        }
    }
    #[test]
    fn missing_texture_transparency_and_invalid_inputs_refuse() {
        let flat = TrackingPlane::from_rgba(&vec![[0.5, 0.5, 0.5, 1.0]; 96 * 72], 96, 72).unwrap();
        assert!(matches!(
            TranslationTracker::new(&flat, [0.5, 0.5], [0.1, 0.1], TrackingConfig::default()),
            Err(TrackingStop::LowTexture)
        ));
        let transparent = TrackingPlane::from_rgba(&vec![[0.0; 4]; 96 * 72], 96, 72).unwrap();
        assert!(tracker().step(&transparent, || false).is_err());
        assert!(TrackingPlane::from_rgba(&[], 0, 0).is_err());
        assert!(TrackingPlane::from_rgba(&[[f32::NAN, 0.0, 0.0, 1.0]], 1, 1).is_err());
        assert!(TrackingPlane::from_rgba(&[[0.0, 0.0, 0.0, 1.1]], 1, 1).is_err());
        assert!(TrackingPlane::from_rgba(&[], 8192, 8192).is_err());
    }
    #[test]
    fn cancellation_and_rejected_frame_do_not_advance_state() {
        let mut t = tracker();
        let mut calls = 0;
        assert_eq!(
            t.step(&texture(2, 1, 1.0, 0.0), || {
                calls += 1;
                calls > 3
            })
            .unwrap_err(),
            TrackingStop::Cancelled
        );
        let a = t.step(&texture(2, 1, 1.0, 0.0), || false).unwrap();
        let b = tracker().step(&texture(2, 1, 1.0, 0.0), || false).unwrap();
        assert_eq!(a.center, b.center);
        assert_eq!(a.score, b.score);
        let tiny = TrackingPlane::from_rgba(&[[1.0; 4]], 1, 1).unwrap();
        assert_eq!(
            t.step(&tiny, || false).unwrap_err(),
            TrackingStop::DimensionsChanged
        );
    }
    #[test]
    fn hard_cut_periodic_ambiguity_and_search_edge_stop() {
        let inverted = texture(0, 0, -1.0, 0.0);
        assert!(matches!(
            tracker().step(&inverted, || false),
            Err(TrackingStop::LowConfidence)
        ));
        assert_eq!(
            tracker()
                .step(&texture(12, 0, 1.0, 0.0), || false)
                .unwrap_err(),
            TrackingStop::SearchLimit
        );
        let mut periodic = vec![[0.0; 4]; 96 * 72];
        for y in 0..72 {
            for x in 0..96 {
                let v = ((x % 4 + y % 4) % 4) as f32;
                periodic[y * 96 + x] = [v, v, v, 1.0];
            }
        }
        let p = TrackingPlane::from_rgba(&periodic, 96, 72).unwrap();
        let mut t =
            TranslationTracker::new(&p, [0.5, 0.5], [0.1, 0.1], TrackingConfig::default()).unwrap();
        assert_eq!(t.step(&p, || false).unwrap_err(), TrackingStop::Ambiguous);
    }
    #[test]
    fn area_downsample_removes_alternating_pixel_alias_and_preserves_signed_hdr() {
        for (low, high) in [(0., 1.), (-2., 10.)] {
            let pixels: Vec<_> = (0..512)
                .map(|x| {
                    let v = if x % 2 == 0 { low } else { high };
                    [v, v, v, 1.]
                })
                .collect();
            let plane = TrackingPlane::from_rgba(&pixels, 512, 1).unwrap();
            assert_eq!(plane.size(), [256, 1]);
            for v in plane.values {
                assert!((v.unwrap() - f64::from((low + high) / 2.)).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn area_downsample_weights_alpha_and_excludes_hidden_rgb() {
        let mut pixels = vec![[0.; 4]; 512 * 2];
        for x in 0..512 {
            pixels[x] = if x % 2 == 0 {
                [0.2, 0.2, 0.2, 1.]
            } else {
                [1., 1., 1., 0.5]
            };
            pixels[512 + x] = if x % 2 == 0 {
                [-0.5, -0.5, -0.5, 1.]
            } else {
                [100., 100., 100., 0.]
            };
        }
        let plane = TrackingPlane::from_rgba(&pixels, 512, 2).unwrap();
        assert_eq!(plane.size(), [256, 1]);
        for v in plane.values {
            assert!((v.unwrap() - 0.28).abs() < 1e-8);
        }
    }

    #[test]
    fn fractional_area_downsample_matches_independent_overlap_oracle() {
        let width = 513;
        let height = 3;
        let pixels: Vec<_> = (0..width * height)
            .map(|i| {
                let v = ((i * 37) % 101) as f32 / 100.;
                [v, v, v, 1.]
            })
            .collect();
        let plane = TrackingPlane::from_rgba(&pixels, width as u32, height as u32).unwrap();
        // 513x3 rounds to256x1; independently integrate every input cell.
        assert_eq!(plane.size(), [256, 1]);
        for x in [0, 1, 127, 255] {
            let lo = x as f64 * 513. / 256.;
            let hi = (x + 1) as f64 * 513. / 256.;
            let mut expected = 0.;
            for sy in 0..height {
                for sx in 0..width {
                    let overlap = (hi.min((sx + 1) as f64) - lo.max(sx as f64)).max(0.);
                    expected += overlap * f64::from(pixels[sy * width + sx][0]);
                }
            }
            expected /= (hi - lo) * 3.;
            assert!((plane.values[x].unwrap() - expected).abs() < 1e-12);
        }
    }

    #[test]
    fn plane_bound_center_offset_and_configuration_are_explicit() {
        let pixels = vec![[0.2, 0.1, 0.3, 0.5]; 1920 * 1080];
        assert_eq!(
            TrackingPlane::from_rgba(&pixels, 1920, 1080)
                .unwrap()
                .size(),
            [256, 144]
        );
        for config in [
            TrackingConfig {
                search_radius: 25,
                ..Default::default()
            },
            TrackingConfig {
                minimum_score: f64::NAN,
                ..Default::default()
            },
            TrackingConfig {
                minimum_margin: -0.1,
                ..Default::default()
            },
        ] {
            assert!(config.validate().is_err());
        }
        let mut t = TranslationTracker::new(
            &texture(0, 0, 1.0, 0.0),
            [0.503, 0.497],
            [0.12, 0.13],
            TrackingConfig::default(),
        )
        .unwrap();
        let got = t.step(&texture(2, 1, 1.0, 0.0), || false).unwrap();
        assert!((got.center[0] - (0.503 + 2.0 / 96.0)).abs() < 0.002);
        assert!((got.center[1] - (0.497 + 1.0 / 72.0)).abs() < 0.002);
    }
    #[test]
    fn bidirectional_translation_preserves_guards_and_rejected_state() {
        let first = texture(0, 0, 1., 0.);
        let mut tracker = BidirectionalTranslationTracker::new(
            &first,
            [0.5, 0.5],
            [0.13, 0.15],
            TrackingConfig::default(),
        )
        .unwrap();
        assert_eq!(
            tracker.step(&texture(2, 1, 1., 0.), || true).unwrap_err(),
            TrackingStop::Cancelled
        );
        let flat = TrackingPlane::from_rgba(&vec![[0.2, 0.2, 0.2, 1.]; 96 * 72], 96, 72).unwrap();
        assert!(tracker.step(&flat, || false).is_err());
        assert_eq!(tracker.center, [0.5, 0.5]);
        for i in 1..=5 {
            let got = tracker.step(&texture(i, -i, 2., -1.), || false).unwrap();
            assert!((got.center[0] - (0.5 + i as f64 / 96.)).abs() < 0.003);
            assert!((got.center[1] - (0.5 - i as f64 / 72.)).abs() < 0.003);
            assert!(got.roundtrip_error_pixels < 1.);
        }
        let at = tracker.center;
        assert!(tracker.step(&texture(30, 0, 1., 0.), || false).is_err());
        assert_eq!(tracker.center, at);
    }

    #[test]
    fn bidirectional_short_term_template_handles_bounded_appearance_change() {
        let appearance = |dx: i32, blend: f64| {
            let mut pixels = vec![[0.; 4]; 96 * 72];
            for y in 0..72i32 {
                for x in 0..96i32 {
                    let xx = x - dx;
                    let v = if (24..72).contains(&xx) && (16..56).contains(&y) {
                        let h =
                            (xx as u32).wrapping_mul(73856093) ^ (y as u32).wrapping_mul(19349663);
                        let k = (xx as u32).wrapping_mul(83492791)
                            ^ (y as u32).wrapping_mul(2971215073);
                        let a = (h % 1009) as f64 / 1009.;
                        let b = (k % 1013) as f64 / 1013.;
                        ((1. - blend) * a + blend * b) as f32
                    } else {
                        0.
                    };
                    pixels[(y * 96 + x) as usize] = [v, v, v, 1.];
                }
            }
            TrackingPlane::from_rgba(&pixels, 96, 72).unwrap()
        };
        let first = appearance(0, 0.);
        let mut adaptive = BidirectionalTranslationTracker::new(
            &first,
            [0.5, 0.5],
            [0.1, 0.12],
            TrackingConfig::default(),
        )
        .unwrap();
        let mut fixed =
            TranslationTracker::new(&first, [0.5, 0.5], [0.1, 0.12], TrackingConfig::default())
                .unwrap();
        let mut fixed_stopped = false;
        for i in 1..=12 {
            let frame = appearance(i, i as f64 / 12.);
            let got = adaptive.step(&frame, || false).unwrap();
            assert!(
                (got.center[0] - (0.5 + i as f64 / 96.)).abs() < 0.006,
                "{i}: {got:?}"
            );
            assert!((got.center[1] - 0.5).abs() < 0.006);
            if !fixed_stopped {
                fixed_stopped = fixed.step(&frame, || false).is_err();
            }
        }
        assert!(
            fixed_stopped,
            "fixture must distinguish fixed and changing references"
        );
    }
    #[test]
    fn bidirectional_static_template_does_not_accumulate_subpixel_offsets() {
        let frame = texture(0, 0, 1., 0.);
        let center = [0.503, 0.497];
        let mut tracker = BidirectionalTranslationTracker::new(
            &frame,
            center,
            [0.13, 0.15],
            TrackingConfig::default(),
        )
        .unwrap();
        for _ in 0..60 {
            let got = tracker.step(&frame, || false).unwrap();
            assert!((got.center[0] - center[0]).abs() * 96. < 0.5);
            assert!((got.center[1] - center[1]).abs() * 72. < 0.5);
        }
    }
}
