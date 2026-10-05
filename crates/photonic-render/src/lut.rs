//! `.cube` 3D-LUT parsing and sampling (07 §3.8, §6.5).
//!
//! A hand-rolled parser for the Adobe/IRIDAS `.cube` format: a `LUT_3D_SIZE N`
//! header plus `N³` whitespace-separated float triples, with optional
//! `LUT_1D_SIZE` / `DOMAIN_MIN` / `DOMAIN_MAX` / `TITLE`. A 1D shaper precedes
//! the 3D table when declared. No external crate is needed.
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
    /// Optional per-channel 1D shaper, sampled before the 3D table.
    pub shaper: Option<Vec<[f32; 3]>>,
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
    /// Invalid or misplaced 1D shaper header.
    BadShaper(String),
    /// Input-domain bounds were non-finite or not strictly increasing.
    BadDomain,
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
            CubeError::BadShaper(s) => write!(f, "bad LUT_1D_SIZE: {s}"),
            CubeError::BadDomain => write!(f, "invalid LUT input domain"),
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
    if it.next().is_some() || v.iter().any(|value| !value.is_finite()) {
        return Err(CubeError::BadTriple(s.to_string()));
    }
    Ok(v)
}

/// Parse a `.cube` file body (already read into a string) into a [`Lut3d`].
///
/// A combined file must declare its 1D table before `LUT_3D_SIZE`; the input
/// domain maps into the shaper, whose output feeds the 3D grid directly.
/// Blank/comment lines and trailing `#` comments are ignored, as is `TITLE`.
pub fn parse_cube(src: &str) -> Result<Lut3d, CubeError> {
    let mut size: Option<usize> = None;
    let mut shaper_size: Option<usize> = None;
    let mut shaper = Vec::new();
    let mut domain_min = [0.0f32; 3];
    let mut domain_max = [1.0f32; 3];
    let mut data: Vec<[f32; 3]> = Vec::new();

    for raw in src.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        // Keyword lines start with an uppercase-letter token.
        let first = line.split_whitespace().next().unwrap_or("");
        match first {
            "TITLE" => {}
            "LUT_3D_SIZE" => {
                if size.is_some() {
                    return Err(CubeError::BadSize("duplicate LUT_3D_SIZE".into()));
                }
                if let Some(expected) = shaper_size {
                    if shaper.len() != expected {
                        return Err(CubeError::WrongCount {
                            expected,
                            got: shaper.len(),
                        });
                    }
                }
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
                if shaper_size.is_some() || size.is_some() || !data.is_empty() {
                    return Err(CubeError::BadShaper("duplicate or misplaced header".into()));
                }
                let n = line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|t| t.parse::<usize>().ok())
                    .ok_or_else(|| CubeError::BadShaper(line.to_string()))?;
                if !(2..=65536).contains(&n) || line.split_whitespace().count() != 2 {
                    return Err(CubeError::BadShaper(line.to_string()));
                }
                shaper_size = Some(n);
            }
            "DOMAIN_MIN" => domain_min = parse_triple(&line[first.len()..])?,
            "DOMAIN_MAX" => domain_max = parse_triple(&line[first.len()..])?,
            _ => {
                let triple = parse_triple(line)?;
                if size.is_some() {
                    data.push(triple);
                } else if let Some(expected) = shaper_size {
                    if shaper.len() == expected {
                        return Err(CubeError::BadShaper("data exceeds declared 1D size".into()));
                    }
                    shaper.push(triple);
                } else {
                    return Err(CubeError::MissingSize);
                }
            }
        }
    }

    let size = size.ok_or(CubeError::MissingSize)?;
    if (0..3).any(|channel| domain_max[channel] <= domain_min[channel]) {
        return Err(CubeError::BadDomain);
    }
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
        shaper: shaper_size.map(|_| shaper),
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
            shaper: None,
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
        if self.shaper.is_some() {
            return v.clamp(0.0, 1.0) * (self.size - 1) as f32;
        }
        let lo = self.domain_min[ch];
        let hi = self.domain_max[ch];
        let span = (hi - lo).max(1e-9);
        let t = ((v - lo) / span).clamp(0.0, 1.0);
        t * (self.size - 1) as f32
    }

    /// Interpolate each shaper channel after mapping the declared input domain.
    #[inline]
    fn shaped(&self, rgb: [f32; 3]) -> [f32; 3] {
        let Some(shaper) = &self.shaper else {
            return rgb;
        };
        let mut out = [0.0; 3];
        for c in 0..3 {
            let t = ((rgb[c] - self.domain_min[c]) / (self.domain_max[c] - self.domain_min[c]))
                .clamp(0.0, 1.0)
                * (shaper.len() - 1) as f32;
            let lo = t.floor() as usize;
            let hi = (lo + 1).min(shaper.len() - 1);
            out[c] = shaper[lo][c] + (shaper[hi][c] - shaper[lo][c]) * (t - lo as f32);
        }
        out
    }

    /// Trilinear sample at input `rgb` (07 §3.8 baseline). Input in the LUT's
    /// declared domain (typically encoded 0..1).
    pub fn sample_trilinear(&self, rgb: [f32; 3]) -> [f32; 3] {
        let rgb = self.shaped(rgb);
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
        let rgb = self.shaped(rgb);
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
    fn combined_cube_applies_shaper_before_3d_table() {
        let with_shaper = format!("LUT_1D_SIZE 2\n0 0 0\n0.5 1 0.25\n{IDENTITY_2}");
        let lut = parse_cube(&with_shaper).unwrap();
        for sample in [
            lut.sample_trilinear([0.4, 0.4, 0.4]),
            lut.sample_tetrahedral([0.4, 0.4, 0.4]),
        ] {
            assert!((sample[0] - 0.2).abs() < 1e-6, "{sample:?}");
            assert!((sample[1] - 0.4).abs() < 1e-6, "{sample:?}");
            assert!((sample[2] - 0.1).abs() < 1e-6, "{sample:?}");
        }
    }

    #[test]
    fn combined_cube_rejects_incomplete_or_misordered_shapers() {
        assert!(matches!(
            parse_cube(&format!("LUT_1D_SIZE 3\n0 0 0\n1 1 1\n{IDENTITY_2}")),
            Err(CubeError::WrongCount {
                expected: 3,
                got: 2
            })
        ));
        assert!(matches!(
            parse_cube(&format!("{IDENTITY_2}LUT_1D_SIZE 2\n")),
            Err(CubeError::BadShaper(_))
        ));
    }

    #[test]
    fn parse_rejects_invalid_domains_and_nonfinite_samples() {
        let reversed = IDENTITY_2.replace("DOMAIN_MAX 1.0 1.0 1.0", "DOMAIN_MAX 0.0 1.0 1.0");
        assert_eq!(parse_cube(&reversed), Err(CubeError::BadDomain));
        let nan = IDENTITY_2.replace("1.0 1.0 1.0\n", "NaN 1.0 1.0\n");
        assert!(matches!(parse_cube(&nan), Err(CubeError::BadTriple(_))));
    }

    #[test]
    fn parse_allows_trailing_comments_without_extra_data_fields() {
        let annotated = IDENTITY_2.replace("1.0 1.0 1.0\n", "1.0 1.0 1.0 # white\n");
        assert_eq!(parse_cube(&annotated).unwrap().data[7], [1.0; 3]);
        let extra = IDENTITY_2.replace("1.0 1.0 1.0\n", "1.0 1.0 1.0 0.5\n");
        assert!(matches!(parse_cube(&extra), Err(CubeError::BadTriple(_))));
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
