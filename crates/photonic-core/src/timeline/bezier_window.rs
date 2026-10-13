//! Bounded closed cubic paths for unassociated grading mattes.
use serde::{Deserialize, Serialize};

pub const MAX_BEZIER_KNOTS: usize = 32;
pub const MAX_BEZIER_SEGMENTS: usize = 512;

/// Absolute normalized frame coordinates. Incoming/outgoing handles belong to
/// this anchor; the closing segment uses the last outgoing and first incoming.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BezierKnot {
    pub anchor: [f32; 2],
    pub incoming: [f32; 2],
    pub outgoing: [f32; 2],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BezierWindow {
    pub knots: Vec<BezierKnot>,
    /// Symmetric half-width, as a fraction of the shorter frame dimension.
    pub feather: f32,
    pub invert: bool,
}

impl Default for BezierWindow {
    fn default() -> Self {
        let k = 0.552_284_8 * 0.25;
        Self {
            knots: vec![
                BezierKnot {
                    anchor: [0.5, 0.25],
                    incoming: [0.5 - k, 0.25],
                    outgoing: [0.5 + k, 0.25],
                },
                BezierKnot {
                    anchor: [0.75, 0.5],
                    incoming: [0.75, 0.5 - k],
                    outgoing: [0.75, 0.5 + k],
                },
                BezierKnot {
                    anchor: [0.5, 0.75],
                    incoming: [0.5 + k, 0.75],
                    outgoing: [0.5 - k, 0.75],
                },
                BezierKnot {
                    anchor: [0.25, 0.5],
                    incoming: [0.25, 0.5 + k],
                    outgoing: [0.25, 0.5 - k],
                },
            ],
            feather: 0.01,
            invert: false,
        }
    }
}

fn midpoint(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5]
}
fn split(c: [[f32; 2]; 4]) -> ([[f32; 2]; 4], [[f32; 2]; 4]) {
    let a = midpoint(c[0], c[1]);
    let b = midpoint(c[1], c[2]);
    let d = midpoint(c[2], c[3]);
    let e = midpoint(a, b);
    let f = midpoint(b, d);
    let m = midpoint(e, f);
    ([c[0], a, e, m], [m, f, d, c[3]])
}
fn segment_distance(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let d = [b[0] - a[0], b[1] - a[1]];
    let v = [p[0] - a[0], p[1] - a[1]];
    let len = d[0] * d[0] + d[1] * d[1];
    let t = if len > 0.0 {
        ((v[0] * d[0] + v[1] * d[1]) / len).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (v[0] - t * d[0]).hypot(v[1] - t * d[1])
}

impl BezierWindow {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(3..=MAX_BEZIER_KNOTS).contains(&self.knots.len()) {
            return Err("Bezier window requires 3..32 knots");
        }
        if self.knots.iter().any(|k| {
            k.anchor
                .iter()
                .chain(&k.incoming)
                .chain(&k.outgoing)
                .any(|v| !v.is_finite() || !(-4.0..=4.0).contains(v))
        }) {
            return Err(
                "Bezier coordinates must be finite and within -4..=4 normalized frame units",
            );
        }
        if !self.feather.is_finite() || !(0.0..=0.25).contains(&self.feather) {
            return Err(
                "Bezier feather must be finite and within 0..=0.25 of the shorter frame dimension",
            );
        }
        let mut distinct = Vec::new();
        for k in &self.knots {
            if !distinct.contains(&k.anchor) {
                distinct.push(k.anchor);
            }
        }
        if distinct.len() < 3 {
            return Err("Bezier window requires at least three distinct anchors");
        }
        Ok(())
    }

    /// Insert a knot without altering the cubic curve (de Casteljau at t=0.5).
    pub fn split_after(&mut self, index: usize) -> Result<(), &'static str> {
        self.validate()?;
        if index >= self.knots.len() || self.knots.len() == MAX_BEZIER_KNOTS {
            return Err("Bezier split index/capacity is invalid");
        }
        let next = (index + 1) % self.knots.len();
        let a = self.knots[index];
        let b = self.knots[next];
        let (left, right) = split([a.anchor, a.outgoing, b.incoming, b.anchor]);
        self.knots[index].outgoing = left[1];
        self.knots[next].incoming = right[2];
        self.knots.insert(
            index + 1,
            BezierKnot {
                anchor: left[3],
                incoming: left[2],
                outgoing: right[1],
            },
        );
        Ok(())
    }

    /// Pixel-space polyline, bounded by 0.25-pixel control-to-chord distance.
    /// Even-odd filling permits concave and self-intersecting closed paths.
    /// Reject rather than silently truncating a path that exceeds the budget.
    pub fn flatten(&self, width: u32, height: u32) -> Result<Vec<[f32; 2]>, &'static str> {
        self.validate()?;
        if width == 0
            || height == 0
            || width > 8192
            || height > 8192
            || u64::from(width) * u64::from(height) > 16_777_216
        {
            return Err("Bezier frame dimensions exceed the 16-megapixel bound");
        }
        let scale = |p: [f32; 2]| [p[0] * width as f32, p[1] * height as f32];
        fn recurse(
            c: [[f32; 2]; 4],
            depth: u8,
            out: &mut Vec<[f32; 2]>,
        ) -> Result<(), &'static str> {
            if segment_distance(c[1], c[0], c[3]).max(segment_distance(c[2], c[0], c[3])) <= 0.25 {
                if out.len() >= MAX_BEZIER_SEGMENTS {
                    return Err("Bezier flattening exceeds 512 segments");
                }
                out.push(c[0]);
                return Ok(());
            }
            if depth == 12 {
                return Err("Bezier flattening exceeds subdivision depth");
            }
            let (a, b) = split(c);
            recurse(a, depth + 1, out)?;
            recurse(b, depth + 1, out)
        }
        let mut out = Vec::new();
        for i in 0..self.knots.len() {
            let a = self.knots[i];
            let b = self.knots[(i + 1) % self.knots.len()];
            recurse(
                [
                    scale(a.anchor),
                    scale(a.outgoing),
                    scale(b.incoming),
                    scale(b.anchor),
                ],
                0,
                &mut out,
            )?;
        }
        if width as u64 * height as u64 * out.len() as u64 > 536_870_912 {
            return Err("Bezier raster exceeds the 512-million pixel-edge work budget");
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bezier_validation_split_and_roundtrip() {
        let mut w = BezierWindow::default();
        let original = w.clone();
        w.split_after(3).unwrap();
        assert_eq!(w.knots.len(), 5);
        assert_eq!(w.knots[0].anchor, original.knots[0].anchor);
        assert_eq!(
            serde_json::from_str::<BezierWindow>(&serde_json::to_string(&w).unwrap()).unwrap(),
            w
        );
        let before = w.clone();
        assert!(w.split_after(99).is_err());
        assert_eq!(w, before);
        for bad in [f32::NAN, f32::INFINITY, 4.1] {
            let mut x = w.clone();
            x.knots[0].outgoing[0] = bad;
            assert!(x.validate().is_err());
        }
        let mut bad = w.clone();
        bad.feather = -0.01;
        assert!(bad.validate().is_err());
        bad.feather = 0.26;
        assert!(bad.validate().is_err());
        bad = original;
        let repeated = bad.knots[0];
        bad.knots.fill(repeated);
        assert!(bad.validate().is_err());
    }
    #[test]
    fn bezier_flatten_is_bounded_and_resolves_curvature() {
        let w = BezierWindow::default();
        let a = w.flatten(64, 32).unwrap();
        let b = w.flatten(1920, 1080).unwrap();
        assert!(b.len() > a.len());
        assert!(b.len() <= MAX_BEZIER_SEGMENTS);
        assert!(w.flatten(0, 1).is_err());
        assert!(w.flatten(8192, 8192).is_err());
        assert!(w.flatten(4096, 2160).is_err());
        let mut split = w.clone();
        split.split_after(1).unwrap();
        assert_eq!(w.flatten(64, 64).unwrap(), split.flatten(64, 64).unwrap());
        let straight = BezierWindow {
            knots: vec![[0.2, 0.2], [0.8, 0.2], [0.8, 0.8], [0.2, 0.8]]
                .into_iter()
                .map(|anchor| BezierKnot {
                    anchor,
                    incoming: anchor,
                    outgoing: anchor,
                })
                .collect(),
            feather: 0.0,
            invert: false,
        };
        assert_eq!(straight.flatten(4096, 2160).unwrap().len(), 4);
    }
}
