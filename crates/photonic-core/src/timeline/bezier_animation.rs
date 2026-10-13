//! Fixed-topology keyframes for closed grading windows. Sampling is pure and
//! resolves to the existing static geometry before render hashing/evaluation.
use super::{BezierWindow, Tick};
use serde::{Deserialize, Serialize};

pub const MAX_BEZIER_KEYS: usize = 128;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BezierWindowInterpolation {
    Hold,
    #[default]
    Linear,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BezierWindowKey {
    pub at: Tick,
    pub window: BezierWindow,
    /// Outgoing segment interpolation; inversion is held until the next key.
    #[serde(default)]
    pub interpolation: BezierWindowInterpolation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BezierWindowTrack {
    #[serde(deserialize_with = "deserialize_keys")]
    pub keys: Vec<BezierWindowKey>,
}

fn deserialize_keys<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<BezierWindowKey>, D::Error> {
    struct Keys;
    impl<'de> serde::de::Visitor<'de> for Keys {
        type Value = Vec<BezierWindowKey>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("at most 128 Bezier keys")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut keys = Vec::new();
            while let Some(key) = seq.next_element()? {
                if keys.len() == MAX_BEZIER_KEYS {
                    return Err(serde::de::Error::custom(
                        "Bezier animation exceeds 128 keys",
                    ));
                }
                keys.push(key);
            }
            Ok(keys)
        }
    }
    deserializer.deserialize_seq(Keys)
}

impl BezierWindowTrack {
    pub fn new(at: Tick, window: BezierWindow) -> Result<Self, &'static str> {
        let track = Self {
            keys: vec![BezierWindowKey {
                at,
                window,
                interpolation: BezierWindowInterpolation::Linear,
            }],
        };
        track.validate()?;
        Ok(track)
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.keys.is_empty() || self.keys.len() > MAX_BEZIER_KEYS {
            return Err("Bezier animation requires 1..128 keys");
        }
        let count = self.keys[0].window.knots.len();
        for (i, key) in self.keys.iter().enumerate() {
            if key.at.0 < 0 || (i > 0 && self.keys[i - 1].at >= key.at) {
                return Err("Bezier key times must be nonnegative and strictly increasing");
            }
            key.window.validate()?;
            if key.window.knots.len() != count {
                return Err("Bezier animation requires fixed knot topology");
            }
        }
        Ok(())
    }
    pub fn sample(&self, at: Tick) -> Result<BezierWindow, &'static str> {
        self.validate()?;
        let next = self.keys.partition_point(|k| k.at <= at);
        if next == 0 {
            return Ok(self.keys[0].window.clone());
        }
        let left = &self.keys[next - 1];
        if next == self.keys.len()
            || left.at == at
            || left.interpolation == BezierWindowInterpolation::Hold
        {
            return Ok(left.window.clone());
        }
        let right = &self.keys[next];
        let numerator = i128::from(at.0) - i128::from(left.at.0);
        let denominator = i128::from(right.at.0) - i128::from(left.at.0);
        let t = numerator as f64 / denominator as f64;
        let mix = |a: f32, b: f32| (a as f64 + (b as f64 - a as f64) * t) as f32;
        let mut out = left.window.clone();
        for (k, r) in out.knots.iter_mut().zip(&right.window.knots) {
            for axis in 0..2 {
                k.anchor[axis] = mix(k.anchor[axis], r.anchor[axis]);
                k.incoming[axis] = mix(k.incoming[axis], r.incoming[axis]);
                k.outgoing[axis] = mix(k.outgoing[axis], r.outgoing[axis]);
            }
        }
        out.feather = mix(out.feather, right.window.feather);
        // Boolean inversion is deliberately held from the left key.
        out.validate()?;
        Ok(out)
    }
    /// Insert a sampled key without altering the existing curve at this tick.
    /// Existing keys are selected by the caller, never silently overwritten.
    pub fn insert_sampled(&mut self, at: Tick) -> Result<bool, &'static str> {
        self.validate()?;
        if at.0 < 0 {
            return Err("Bezier key time must be nonnegative");
        }
        match self.keys.binary_search_by_key(&at, |k| k.at) {
            Ok(_) => Ok(false),
            Err(index) => {
                if self.keys.len() == MAX_BEZIER_KEYS {
                    return Err("Bezier animation exceeds 128 keys");
                }
                let window = self.sample(at)?;
                let interpolation = if index > 0 {
                    self.keys[index - 1].interpolation
                } else {
                    BezierWindowInterpolation::Linear
                };
                self.keys.insert(
                    index,
                    BezierWindowKey {
                        at,
                        window,
                        interpolation,
                    },
                );
                Ok(true)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn moved(dx: f32) -> BezierWindow {
        let mut w = BezierWindow::default();
        for k in &mut w.knots {
            for p in [&mut k.anchor, &mut k.incoming, &mut k.outgoing] {
                p[0] += dx;
            }
        }
        w
    }
    #[test]
    fn animated_bezier_variant_preserves_static_json_and_has_explicit_identity() {
        use crate::timeline::GradeGraphNode;
        let old = serde_json::json!({"kind":"bezier_matte","input":0,"window":BezierWindow::default(),"label":"Static"});
        let loaded: GradeGraphNode = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(serde_json::to_value(loaded).unwrap(), old);
        let animated = GradeGraphNode::AnimatedBezierMatte {
            input: 0,
            track: BezierWindowTrack::new(Tick(0), BezierWindow::default()).unwrap(),
            label: "Animated".into(),
        };
        let saved = serde_json::to_value(&animated).unwrap();
        assert_eq!(saved["kind"], "animated_bezier_matte");
        assert_eq!(
            serde_json::from_value::<GradeGraphNode>(saved.clone()).unwrap(),
            animated
        );
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        #[allow(dead_code)]
        enum PriorReader {
            BezierMatte {
                input: u32,
                window: BezierWindow,
                label: String,
            },
        }
        assert!(serde_json::from_value::<PriorReader>(saved).is_err());
    }

    #[test]
    fn bezier_keys_sample_endpoints_hold_linear_and_large_integer_time() {
        let start = i64::MAX - 10;
        let mut t = BezierWindowTrack::new(Tick(start), moved(0.0)).unwrap();
        let mut end = moved(0.2);
        end.feather = 0.11;
        end.invert = true;
        t.keys.push(BezierWindowKey {
            at: Tick(start + 10),
            window: end.clone(),
            interpolation: BezierWindowInterpolation::Linear,
        });
        assert_eq!(t.sample(Tick(-1)).unwrap(), moved(0.0));
        let mid = t.sample(Tick(start + 5)).unwrap();
        assert!((mid.knots[0].anchor[0] - 0.6).abs() < 1e-6);
        assert!((mid.feather - 0.06).abs() < 1e-6);
        assert!(!mid.invert);
        assert_eq!(t.sample(Tick(i64::MAX)).unwrap(), end);
        t.keys[0].interpolation = BezierWindowInterpolation::Hold;
        assert_eq!(t.sample(Tick(start + 9)).unwrap(), moved(0.0));
        assert_eq!(t.sample(Tick(i64::MAX)).unwrap(), end);
    }
    #[test]
    fn bezier_keys_reject_bad_times_topology_capacity_and_degenerate_sample() {
        let good = BezierWindowTrack::new(Tick(0), moved(0.0)).unwrap();
        let mut bad = good.clone();
        bad.keys[0].at = Tick(-1);
        assert!(bad.validate().is_err());
        bad = good.clone();
        bad.keys.push(bad.keys[0].clone());
        assert!(bad.validate().is_err());
        bad = good.clone();
        let mut last = bad.keys[0].clone();
        last.at = Tick(10);
        last.window.split_after(0).unwrap();
        bad.keys.push(last);
        assert!(bad.validate().is_err());
        bad = good.clone();
        let mut last = bad.keys[0].clone();
        last.at = Tick(10);
        for k in &mut last.window.knots {
            for p in [&mut k.anchor, &mut k.incoming, &mut k.outgoing] {
                p[0] = 1.0 - p[0];
                p[1] = 1.0 - p[1];
            }
        }
        bad.keys.push(last);
        assert!(bad.validate().is_ok());
        assert!(bad.sample(Tick(5)).is_err());
        let json = serde_json::json!({"keys":vec![good.keys[0].clone();MAX_BEZIER_KEYS+1]});
        assert!(serde_json::from_value::<BezierWindowTrack>(json).is_err());
    }
    #[test]
    fn bezier_key_insertion_is_atomic_preserves_samples_and_roundtrips() {
        let mut t = BezierWindowTrack::new(Tick(0), moved(0.0)).unwrap();
        t.keys.push(BezierWindowKey {
            at: Tick(10),
            window: moved(0.2),
            interpolation: BezierWindowInterpolation::Linear,
        });
        let original = t.clone();
        let samples: Vec<_> = (0..=10).map(|n| t.sample(Tick(n)).unwrap()).collect();
        assert!(t.insert_sampled(Tick(5)).unwrap());
        assert!(!t.insert_sampled(Tick(5)).unwrap());
        for (n, w) in samples.iter().enumerate() {
            let now = t.sample(Tick(n as i64)).unwrap();
            for (a, b) in now.knots.iter().zip(&w.knots) {
                for (a, b) in a
                    .anchor
                    .iter()
                    .chain(&a.incoming)
                    .chain(&a.outgoing)
                    .zip(b.anchor.iter().chain(&b.incoming).chain(&b.outgoing))
                {
                    assert!((a - b).abs() < 1e-6);
                }
            }
        }
        let saved = serde_json::to_string(&t).unwrap();
        assert_eq!(
            serde_json::from_str::<BezierWindowTrack>(&saved).unwrap(),
            t
        );
        let before = t.clone();
        assert!(t.insert_sampled(Tick(-1)).is_err());
        assert_eq!(t, before);
        assert_ne!(t, original);
    }
}
