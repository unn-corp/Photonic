//! Opt-in observation on externally supplied, provenance-recorded footage.
//! No footage is bundled and a completed trace is not an accuracy claim.
use photonic_render::window_tracking::{
    BidirectionalTranslationTracker, TrackingConfig, TrackingPlane, TranslationTracker,
};
use serde_json::json;
use std::path::Path;

fn plane(path: &Path, nearest_baseline: bool) -> TrackingPlane {
    let image = image::open(path).expect("read supplied PNG").to_rgba8();
    let linear = |v: u8| {
        let x = f32::from(v) / 255.;
        if x <= 0.04045 {
            x / 12.92
        } else {
            ((x + 0.055) / 1.055).powf(2.4)
        }
    };
    let pixels: Vec<_> = image
        .pixels()
        .map(|p| {
            let a = f32::from(p[3]) / 255.;
            [linear(p[0]) * a, linear(p[1]) * a, linear(p[2]) * a, a]
        })
        .collect();
    if nearest_baseline {
        let scale = (256. / image.width().max(image.height()) as f64).min(1.);
        let w = (image.width() as f64 * scale).round().max(1.) as u32;
        let h = (image.height() as f64 * scale).round().max(1.) as u32;
        let mut nearest = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                let sx = (((x as f64 + 0.5) * image.width() as f64 / w as f64).floor() as u32)
                    .min(image.width() - 1);
                let sy = (((y as f64 + 0.5) * image.height() as f64 / h as f64).floor() as u32)
                    .min(image.height() - 1);
                nearest.push(pixels[(sy * image.width() + sx) as usize]);
            }
        }
        return TrackingPlane::from_rgba(&nearest, w, h).unwrap();
    }
    TrackingPlane::from_rgba(&pixels, image.width(), image.height()).unwrap()
}

#[test]
#[ignore = "requires external 31-frame OpenCV vtest sample and explicit provenance; observation only"]
fn real_footage_translation_tracking_observation() {
    let root = std::path::PathBuf::from(
        std::env::var_os("PHOTONIC_TRACKING_FOOTAGE_DIR").expect("set external footage directory"),
    );
    let provenance: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("source.json")).unwrap()).unwrap();
    assert_eq!(
        provenance["sha256"],
        "45cddc9490be69345cbdab64ca583be65987e864ca408038e648db99e10516cf"
    );
    let mut reports = Vec::new();
    for method in ["nearest_baseline", "area_filtered", "bidirectional_area"] {
        let nearest_baseline = method == "nearest_baseline";
        let first = plane(&root.join("frames/001.png"), nearest_baseline);
        for (name, center, size, static_control) in [
            (
                "fixed_window",
                [450. / 768., 55. / 576.],
                [0.02, 0.035],
                true,
            ),
            (
                "sign_with_passing_occluders",
                [428. / 768., 225. / 576.],
                [0.02, 0.035],
                false,
            ),
            (
                "left_walker",
                [267. / 768., 257. / 576.],
                [0.02, 0.035],
                false,
            ),
            (
                "middle_walker",
                [514. / 768., 190. / 576.],
                [0.02, 0.035],
                false,
            ),
            (
                "right_walker",
                [666. / 768., 278. / 576.],
                [0.02, 0.035],
                false,
            ),
        ] {
            let mut tracker =
                TranslationTracker::new(&first, center, size, TrackingConfig::default()).unwrap();
            let mut adaptive = BidirectionalTranslationTracker::new(
                &first,
                center,
                size,
                TrackingConfig::default(),
            )
            .unwrap();
            let mut samples = vec![json!({"frame":0,"center":center,"score":1.0})];
            let mut stopped = None;
            for index in 1..31 {
                let frame = plane(
                    &root.join(format!("frames/{:03}.png", index + 1)),
                    nearest_baseline,
                );
                let result = if method == "bidirectional_area" {
                    adaptive
                        .step(&frame, || false)
                        .map(|found| serde_json::to_value(found).unwrap())
                } else {
                    tracker
                        .step(&frame, || false)
                        .map(|found| serde_json::to_value(found).unwrap())
                };
                match result {
                    Ok(mut found) => {
                        found["frame"] = json!(index);
                        samples.push(found);
                    }
                    Err(reason) => {
                        stopped = Some(json!({"frame":index,"reason":reason}));
                        break;
                    }
                }
            }
            if static_control {
                assert_eq!(samples.len(), 31, "static control stopped: {stopped:?}");
                for sample in &samples {
                    let at = sample["center"].as_array().unwrap();
                    assert!((at[0].as_f64().unwrap() - center[0]).abs() * 256. < 1.0);
                    assert!((at[1].as_f64().unwrap() - center[1]).abs() * 192. < 1.0);
                }
            }
            reports.push(json!({"sampling":method,"name":name,"initial_center":center,"half_size":size,"completed":stopped.is_none(),"stopped":stopped,"samples":samples,"accuracy_ground_truth":if static_control{"fixed landmark, within one analysis pixel"}else{"none; inspect trace, completion is not accuracy"}}));
        }
    }
    let result = json!({"source":provenance,"input_domain":"linearized RGB texture signal; CPU tracking core only, not native video decode/graph qualification","analysis_size":[256,192],"default_thresholds":true,"observations":reports});
    std::fs::write(
        root.join("tracking-observation.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
}
