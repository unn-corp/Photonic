//! Report why a clip can or cannot be gyro-stabilized (D-12).
//!
//! `cargo run -p photonic-video --example d12_diag -- <file>...`
//!
//! Exists because "this clip won't stabilize" has several very different
//! causes — a re-encoded copy, a consumer drone's low-rate flight log, an
//! unsupported dialect — and telling them apart from the GUI's one-line status
//! is slow. Point this at the files in question and it names the actual reason.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: d12_diag <media-or-sidecar>...");
        std::process::exit(2);
    }
    for a in args {
        let p = std::path::PathBuf::from(&a);
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| a.clone());
        match photonic_video::media::parse_motion(&p) {
            Ok(s) => {
                println!("[usable] {name}");
                println!(
                    "         {} samples, format {:?}",
                    s.samples.len(),
                    s.format
                );
                if let Some(hz) = s.sample_rate_hz() {
                    println!("         {hz:.0} Hz");
                }
                println!(
                    "         accelerometer: {}",
                    if s.has_accel() {
                        "yes (horizon lock available)"
                    } else {
                        "no (horizon lock will do nothing)"
                    }
                );
                if s.dropped_invalid > 0 {
                    println!("         {} non-finite samples dropped", s.dropped_invalid);
                }
            }
            Err(e) => {
                println!("[no gyro] {name}");
                println!("          {e}");
            }
        }
        println!();
    }
}
