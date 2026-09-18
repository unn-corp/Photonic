//! `.cube` 3D-LUT parsing and sampling (07 §3.8, §6.5).
//!
//! A hand-rolled parser for the Adobe/IRIDAS `.cube` format: a `LUT_3D_SIZE N`
//! header plus `N³` whitespace-separated float triples, with optional
//! `LUT_1D_SIZE` / `DOMAIN_MIN` / `DOMAIN_MAX` / `TITLE`. No external crate is
//! needed — the grammar is line-oriented and tiny.
//!
//! **Documented deviation from 07 §3.8:** the spec placed the `.cube` parser
//! engine-side (`photonic-video`). We keep the parser *and* its trilinear /
//! tetrahedral sampler together here in `photonic-render` so the parse→sample
//! path is one unit with GPU/CPU parity coverage, and so the resolved
//! [`crate::grade::ResolvedGradeOp`] can carry a ready-to-sample table. The
//! engine still owns *asset I/O* (reading the file bytes off disk and relinking
//! by hash); it hands the bytes to [`parse_cube`]. Flagged for the P7 gate.

/// A parsed 3D LUT: a cube of `size³` RGB samples plus its input domain.
///
/// Entries are stored in `.cube` order — **red varies fastest**, then green,
/// then blue: `data[r + g*size + b*size*size]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut3d {
    /// Grid resolution `N` (`LUT_3D_SIZE`), 2..=256.
    pub size: usize,
    /// `size³` RGB triples, red-fastest order.
    pub data: Vec<[f32; 3]>,
    /// Per-channel input-domain lower bound (`DOMAIN_MIN`, default 0).
    pub domain_min: [f32; 3],
    /// Per-channel input-domain upper bound (`DOMAIN_MAX`, default 1).
    pub domain_max: [f32; 3],
}

/// Error parsing a `.cube` file.
#[derive(Debug, Clone, PartialEq)]
pub enum CubeError {
    /// No `LUT_3D_SIZE` header was found.
    MissingSize,
    /// `LUT_3D_SIZE` was absent, zero, out of the 2..=256 range, or unparsable.
    BadSize(String),
    /// A data row did not contain three parsable floats.
    BadTriple(String),
    /// The data-row count did not equal `size³`.
    WrongCount { expected: usize, got: usize },
}

impl std::fmt::Display for CubeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CubeError::MissingSize => write!(f, "no LUT_3D_SIZE header"),
            CubeError::BadSize(s) => write!(f, "bad LUT_3D_SIZE: {s}"),
            CubeError::BadTriple(s) => write!(f, "bad data row: {s}"),
            CubeError::WrongCount { expected, got } => {
                write!(f, "expected {expected} entries, got {got}")
            }
        }
    }
}

impl std::error::Error for CubeError {}

fn parse_triple(s: &str) -> Result<[f32; 3], CubeError> {
    let mut it = s.split_whitespace();
    let mut v = [0.0f32; 3];
    for slot in v.iter_mut() {
        let tok = it
            .next()
            .ok_or_else(|| CubeError::BadTriple(s.to_string()))?;
        *slot = tok
            .parse::<f32>()
            .map_err(|_| CubeError::BadTriple(s.to_string()))?;
    }
    Ok(v)
}

/// Parse a `.cube` file body (already read into a string) into a [`Lut3d`].
///
/// `LUT_1D_SIZE` shaper LUTs are recognized (their rows skipped) but not
/// applied — v1 grades against the 3D table only (07 §3.8). Lines starting with
/// `#` and blank lines are ignored, as are `TITLE`.
pub fn parse_cube(src: &str) -> Result<Lut3d, CubeError> {
    let mut size: Option<usize> = None;
    let mut lut_1d_size: Option<usize> = None;
    let mut domain_min = [0.0f32; 3];
    let mut domain_max = [1.0f32; 3];
    let mut data: Vec<[f32; 3]> = Vec::new();

    for raw in src.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Keyword lines start with an uppercase-letter token.
        let first = line.split_whitespace().next().unwrap_or("");
        match first {
            "TITLE" => {}
            "LUT_3D_SIZE" => {
                let n = line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|t| t.parse::<usize>().ok())
                    .ok_or_else(|| CubeError::BadSize(line.to_string()))?;
                if !(2..=256).contains(&n) {
                    return Err(CubeError::BadSize(line.to_string()));
                }
                size = Some(n);
            }
            "LUT_1D_SIZE" => {
                lut_1d_size = line.split_whitespace().nth(1).and_then(|t| t.parse().ok());
            }
            "DOMAIN_MIN" => domain_min = parse_triple(&line[first.len()..])?,
            "DOMAIN_MAX" => domain_max = parse_triple(&line[first.len()..])?,
            _ => {
                // A data row: three floats. Skip 1D-shaper rows if a 1D header
                // was declared and we have not yet reached the 3D block.
                let triple = parse_triple(line)?;
                if let (Some(n1), None) = (lut_1d_size, size) {
                    // Still inside the 1D block (before LUT_3D_SIZE) — consume
                    // and discard `n1` rows' worth by tracking count implicitly.
                    let _ = n1;
                    continue;
                }
                data.push(triple);
            }
        }
    }

    let size = size.ok_or(CubeError::MissingSize)?;
    let expected = size * size * size;
    if data.len() != expected {
        return Err(CubeError::WrongCount {
            expected,
            got: data.len(),
        });
    }
    Ok(Lut3d {
        size,
        data,
        domain_min,
        domain_max,
    })
}

impl Lut3d {
    /// The identity LUT of resolution `size`: each grid node maps to its own
    /// normalized coordinate. Sampling it (either interpolation mode) returns
    /// the input unchanged.
    pub fn identity(size: usize) -> Lut3d {
        let n = size.max(2);
        let mut data = Vec::with_capacity(n * n * n);
        let denom = (n - 1) as f32;
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    data.push([r as f32 / denom, g as f32 / denom, b as f32 / denom]);
                }
            }
        }
        Lut3d {
            size: n,
            data,
            domain_min: [0.0; 3],
            domain_max: [1.0; 3],
        }
    }

    #[inline]
    fn at(&self, r: usize, g: usize, b: usize) -> [f32; 3] {
        self.data[r + g * self.size + b * self.size * self.size]
    }

    /// Map an input channel from `[domain_min, domain_max]` onto grid coords
    /// `[0, size-1]`, clamped.
    #[inline]
    fn grid_coord(&self, v: f32, ch: usize) -> f32 {
        let lo = self.domain_min[ch];
        let hi = self.domain_max[ch];
        let span = (hi - lo).max(1e-9);
        let t = ((v - lo) / span).clamp(0.0, 1.0);
        t * (self.size - 1) as f32
    }

    /// Trilinear sample at input `rgb` (07 §3.8 baseline). Input in the LUT's
    /// declared domain (typically encoded 0..1).
    pub fn sample_trilinear(&self, rgb: [f32; 3]) -> [f32; 3] {
        let fx = self.grid_coord(rgb[0], 0);
        let fy = self.grid_coord(rgb[1], 1);
        let fz = self.grid_coord(rgb[2], 2);
        let (x0, y0, z0) = (
            fx.floor() as usize,
            fy.floor() as usize,
            fz.floor() as usize,
        );
        let max = self.size - 1;
        let x1 = (x0 + 1).min(max);
        let y1 = (y0 + 1).min(max);
        let z1 = (z0 + 1).min(max);
        let (dx, dy, dz) = (fx - x0 as f32, fy - y0 as f32, fz - z0 as f32);
        let c000 = self.at(x0, y0, z0);
        let c100 = self.at(x1, y0, z0);
        let c010 = self.at(x0, y1, z0);
        let c110 = self.at(x1, y1, z0);
        let c001 = self.at(x0, y0, z1);
        let c101 = self.at(x1, y0, z1);
        let c011 = self.at(x0, y1, z1);
        let c111 = self.at(x1, y1, z1);
        let mut out = [0.0f32; 3];
        for c in 0..3 {
            let c00 = c000[c] + (c100[c] - c000[c]) * dx;
            let c10 = c010[c] + (c110[c] - c010[c]) * dx;
            let c01 = c001[c] + (c101[c] - c001[c]) * dx;
            let c11 = c011[c] + (c111[c] - c011[c]) * dx;
            let c0 = c00 + (c10 - c00) * dy;
            let c1 = c01 + (c11 - c01) * dy;
            out[c] = c0 + (c1 - c0) * dz;
        }
        out
    }

    /// Tetrahedral sample at input `rgb` (07 §3.8, §6.5 quality mode). More
    /// accurate than trilinear near LUT-grid edges; identical on the neutral
    /// diagonal. Uses the canonical 6-tetrahedron decomposition of the cell.
    pub fn sample_tetrahedral(&self, rgb: [f32; 3]) -> [f32; 3] {
        let fx = self.grid_coord(rgb[0], 0);
        let fy = self.grid_coord(rgb[1], 1);
        let fz = self.grid_coord(rgb[2], 2);
        let (x0, y0, z0) = (
            fx.floor() as usize,
            fy.floor() as usize,
            fz.floor() as usize,
        );
        let max = self.size - 1;
        let x1 = (x0 + 1).min(max);
        let y1 = (y0 + 1).min(max);
        let z1 = (z0 + 1).min(max);
        let (dr, dg, db) = (fx - x0 as f32, fy - y0 as f32, fz - z0 as f32);

        let c000 = self.at(x0, y0, z0);
        let c100 = self.at(x1, y0, z0);
        let c010 = self.at(x0, y1, z0);
        let c110 = self.at(x1, y1, z0);
        let c001 = self.at(x0, y0, z1);
        let c101 = self.at(x1, y0, z1);
        let c011 = self.at(x0, y1, z1);
        let c111 = self.at(x1, y1, z1);

        let mut out = [0.0f32; 3];
        for c in 0..3 {
            // Six tetrahedra, selected by the ordering of (dr, dg, db). Each
            // branch is out = c000 + Σ weight·(edge delta).
            let v = if dr > dg {
                if dg > db {
                    c000[c]
                        + dr * (c100[c] - c000[c])
                        + dg * (c110[c] - c100[c])
                        + db * (c111[c] - c110[c])
                } else if dr > db {
                    c000[c]
                        + dr * (c100[c] - c000[c])
                        + db * (c101[c] - c100[c])
                        + dg * (c111[c] - c101[c])
                } else {
                    c000[c]
                        + db * (c001[c] - c000[c])
                        + dr * (c101[c] - c001[c])
                        + dg * (c111[c] - c101[c])
                }
            } else if db > dg {
                c000[c]
                    + db * (c001[c] - c000[c])
                    + dg * (c011[c] - c001[c])
                    + dr * (c111[c] - c011[c])
            } else if db > dr {
                c000[c]
                    + dg * (c010[c] - c000[c])
                    + db * (c011[c] - c010[c])
                    + dr * (c111[c] - c011[c])
            } else {
                c000[c]
                    + dg * (c010[c] - c000[c])
                    + dr * (c110[c] - c010[c])
                    + db * (c111[c] - c110[c])
            };
            out[c] = v;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY_2: &str = "\
# a tiny identity LUT
TITLE \"id\"
LUT_3D_SIZE 2
DOMAIN_MIN 0.0 0.0 0.0
DOMAIN_MAX 1.0 1.0 1.0
0.0 0.0 0.0
1.0 0.0 0.0
0.0 1.0 0.0
1.0 1.0 0.0
0.0 0.0 1.0
1.0 0.0 1.0
0.0 1.0 1.0
1.0 1.0 1.0
";

    #[test]
    fn parse_identity_cube() {
        let lut = parse_cube(IDENTITY_2).unwrap();
        assert_eq!(lut.size, 2);
        assert_eq!(lut.data.len(), 8);
        assert_eq!(lut.data[0], [0.0, 0.0, 0.0]);
        assert_eq!(lut.data[7], [1.0, 1.0, 1.0]);
    }

    #[test]
    fn parse_rejects_wrong_count() {
        let bad = "LUT_3D_SIZE 2\n0 0 0\n1 0 0\n";
        assert!(matches!(
            parse_cube(bad),
            Err(CubeError::WrongCount {
                expected: 8,
                got: 2
            })
        ));
    }

    #[test]
    fn parse_rejects_missing_size() {
        assert_eq!(parse_cube("0 0 0\n"), Err(CubeError::MissingSize));
    }

    #[test]
    fn identity_samples_return_input_both_modes() {
        let lut = parse_cube(IDENTITY_2).unwrap();
        for p in [
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.25, 0.5, 0.75],
            [0.9, 0.1, 0.4],
        ] {
            let tri = lut.sample_trilinear(p);
            let tet = lut.sample_tetrahedral(p);
            for c in 0..3 {
                assert!((tri[c] - p[c]).abs() < 1e-6, "trilinear {tri:?} vs {p:?}");
                assert!((tet[c] - p[c]).abs() < 1e-6, "tetrahedral {tet:?} vs {p:?}");
            }
        }
    }

    #[test]
    fn trilinear_reproduces_multilinear_known_value() {
        // A size-2 LUT whose red output = r, green = g, blue = 0.5*b is exactly
        // multilinear, so trilinear reproduces it everywhere. At (0.5,0.25,0.8):
        // expected [0.5, 0.25, 0.4].
        let mut lut = Lut3d::identity(2);
        for e in lut.data.iter_mut() {
            e[2] *= 0.5;
        }
        let out = lut.sample_trilinear([0.5, 0.25, 0.8]);
        assert!((out[0] - 0.5).abs() < 1e-6, "r {out:?}");
        assert!((out[1] - 0.25).abs() < 1e-6, "g {out:?}");
        assert!((out[2] - 0.4).abs() < 1e-6, "b {out:?}");
    }

    #[test]
    fn invert_lut_maps_complement() {
        // Corner values = 1 - position → a pure inversion. Multilinear, so
        // trilinear is exact off-grid too.
        let mut lut = Lut3d::identity(2);
        for e in lut.data.iter_mut() {
            for v in e.iter_mut() {
                *v = 1.0 - *v;
            }
        }
        let out = lut.sample_trilinear([0.3, 0.6, 0.9]);
        assert!((out[0] - 0.7).abs() < 1e-6);
        assert!((out[1] - 0.4).abs() < 1e-6);
        assert!((out[2] - 0.1).abs() < 1e-6);
    }

    #[test]
    fn domain_remaps_input() {
        // Domain 0..2 halves the effective input coordinate. Identity table over
        // 0..2 means input 1.0 lands at grid centre → output 0.5.
        let mut lut = Lut3d::identity(2);
        lut.domain_max = [2.0, 2.0, 2.0];
        let out = lut.sample_trilinear([1.0, 1.0, 1.0]);
        for c in 0..3 {
            assert!((out[c] - 0.5).abs() < 1e-6, "{out:?}");
        }
    }

    #[test]
    fn parse_channel_swap_fixture_exact_sample() {
        // 29 §6 gap 2 / CAP-015: the checked-in test LUT is a pure RGB→GBR
        // channel swap at LUT_3D_SIZE 2. Because the transform is a coordinate
        // permutation (linear), trilinear interpolation reproduces it exactly,
        // so the fixture yields byte-level assertions in AS-2, not tolerances.
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../photonic-video/tests/fixtures/channel_swap_rgb_to_gbr.cube"
        );
        let src = std::fs::read_to_string(path).expect("read channel-swap .cube fixture");
        let lut = parse_cube(&src).expect("parse channel-swap .cube fixture");
        assert_eq!(lut.size, 2);
        assert_eq!(lut.domain_min, [0.0, 0.0, 0.0]);
        assert_eq!(lut.domain_max, [1.0, 1.0, 1.0]);
        // (r,g,b) -> (g,b,r). Sampling (0.25, 0.5, 0.75) -> (0.5, 0.75, 0.25).
        let out = lut.sample_trilinear([0.25, 0.5, 0.75]);
        assert!((out[0] - 0.5).abs() < 1e-6, "r {out:?}");
        assert!((out[1] - 0.75).abs() < 1e-6, "g {out:?}");
        assert!((out[2] - 0.25).abs() < 1e-6, "b {out:?}");
        // A grid corner confirms the permutation directly: input (1,0,0) is the
        // pure-red node, whose GBR image is pure blue (0,0,1).
        let corner = lut.sample_trilinear([1.0, 0.0, 0.0]);
        assert_eq!(corner, [0.0, 0.0, 1.0]);
    }
}
