//! K-A6 Edit Duration dialog — frame-accurate numeric form for position /
//! source in / source out / duration on the selected timeline clip, with a
//! **ripple** checkbox.
//!
//! Routes exclusively through [`crate::app::timeline::ops_bridge::edit_duration`]
//! (which calls pure `ops::edit_clip_timing`). Never mutates `doc.timeline`
//! directly.

use crate::app::timeline::ops_bridge;
use egui::{Color32, RichText};
use photonic_core::document::Document;
use photonic_core::history::CommandHistory;
use photonic_core::timeline::{
    ClipId, ClipTiming, FrameRate, SequenceId, Tick, Timecode, TrackId, TICKS_PER_SECOND,
};

const MUTED: Color32 = Color32::from_rgb(0x7A, 0x7A, 0x9A);

/// Session state for the open Edit Duration dialog (K-A6).
#[derive(Clone, Debug)]
pub(crate) struct EditDurationDialog {
    pub seq: SequenceId,
    pub track: TrackId,
    pub clip: ClipId,
    /// Timeline start as timecode / frames text.
    pub position: String,
    /// Source in as timecode / frames text.
    pub source_in: String,
    /// Timeline duration as timecode / frames text.
    pub duration: String,
    /// When true, duration changes that hold start fixed ripple later clips.
    pub ripple: bool,
    /// Last apply error message (parse or ops rejection).
    pub error: Option<String>,
}

impl EditDurationDialog {
    /// Seed fields from the live clip at `(seq, track, clip)`.
    pub fn seed(doc: &Document, seq: SequenceId, track: TrackId, clip: ClipId) -> Option<Self> {
        let project = doc.timeline.as_ref()?;
        let sequence = project.sequences.get(&seq)?;
        let c = sequence.track(track)?.clips.iter().find(|c| c.id == clip)?;
        let rate = sequence.frame_rate;
        let start_tc = sequence.start_timecode;
        Some(Self {
            seq,
            track,
            clip,
            position: format_field(c.start, rate, start_tc),
            source_in: format_field(c.source_in, rate, Tick::ZERO),
            duration: format_field(c.duration, rate, Tick::ZERO),
            ripple: false,
            error: None,
        })
    }
}

fn format_field(tick: Tick, rate: FrameRate, start: Tick) -> String {
    Timecode::format_tick(tick, rate, start, rate.is_drop_frame_rate())
}

/// Parse a field as either SMPTE TC (`HH:MM:SS:FF` / `;`) or a bare frame count.
/// `start_offset` is subtracted after TC parse (sequence start for position).
fn parse_field(text: &str, rate: FrameRate, start_offset: Tick) -> Result<Tick, String> {
    let t = text.trim();
    if t.is_empty() {
        return Err("empty value".into());
    }
    // Bare integer = frame count from the field's zero (position: sequence zero
    // before start_timecode offset; duration / source_in: absolute frames).
    if let Ok(frames) = t.parse::<i64>() {
        if frames < 0 {
            return Err("negative frame count".into());
        }
        return Ok(Tick(frames.saturating_mul(rate.ticks_per_frame().0)));
    }
    let parsed = Timecode::parse_to_tick(t, rate)
        .ok_or_else(|| format!("could not parse `{t}` (use HH:MM:SS:FF or a frame count)"))?;
    Ok(Tick(parsed.0.saturating_sub(start_offset.0).max(0)))
}

/// Draw the floating Edit Duration window when `state` is `Some`.
/// Clears `state` on Cancel / close / successful Apply.
pub(crate) fn draw_edit_duration_dialog(
    ctx: &egui::Context,
    doc: &mut Document,
    history: &mut CommandHistory,
    state: &mut Option<EditDurationDialog>,
) {
    let Some(dlg) = state.as_mut() else {
        return;
    };

    let (rate, start_tc, source_out_preview) = {
        let Some(project) = doc.timeline.as_ref() else {
            *state = None;
            return;
        };
        let Some(seq) = project.sequences.get(&dlg.seq) else {
            *state = None;
            return;
        };
        let rate = seq.frame_rate;
        let start_tc = seq.start_timecode;
        // Live source-out preview from the draft duration + source_in fields.
        let source_out = match (
            parse_field(&dlg.source_in, rate, Tick::ZERO),
            parse_field(&dlg.duration, rate, Tick::ZERO),
        ) {
            (Ok(sin), Ok(dur)) => {
                // Out ≈ source_in + duration (1× speed; ramps stay a follow-up).
                Some(format_field(sin + dur, rate, Tick::ZERO))
            }
            _ => None,
        };
        (rate, start_tc, source_out)
    };

    let mut open = true;
    let mut apply = false;
    let mut cancel = false;

    egui::Window::new("Edit Duration")
        .id(egui::Id::new("k_a6_edit_duration"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(320.0)
        .show(ctx, |ui| {
            ui.label(
                RichText::new("Frame-accurate position / in / out / duration (K-A6)")
                    .color(MUTED)
                    .small(),
            );
            ui.add_space(6.0);

            egui::Grid::new("edit_duration_grid")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Position");
                    ui.add(
                        egui::TextEdit::singleline(&mut dlg.position)
                            .desired_width(180.0)
                            .hint_text("HH:MM:SS:FF or frames"),
                    );
                    ui.end_row();

                    ui.label("Source In");
                    ui.add(
                        egui::TextEdit::singleline(&mut dlg.source_in)
                            .desired_width(180.0)
                            .hint_text("HH:MM:SS:FF or frames"),
                    );
                    ui.end_row();

                    ui.label("Source Out");
                    ui.label(
                        RichText::new(source_out_preview.as_deref().unwrap_or("—"))
                            .color(MUTED)
                            .monospace(),
                    );
                    ui.end_row();

                    ui.label("Duration");
                    ui.add(
                        egui::TextEdit::singleline(&mut dlg.duration)
                            .desired_width(180.0)
                            .hint_text("HH:MM:SS:FF or frames"),
                    );
                    ui.end_row();
                });

            ui.add_space(4.0);
            ui.checkbox(&mut dlg.ripple, "Ripple")
                .on_hover_text(
                    "When checked, changing duration (with position held) shifts later clips on the track",
                );

            if let Some(err) = &dlg.error {
                ui.colored_label(Color32::from_rgb(0xE5, 0x4D, 0x2E), err);
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Apply").clicked() {
                    apply = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
            ui.label(
                RichText::new(format!(
                    "Rate {:.3} fps · {} ticks/s",
                    rate.num as f64 / rate.den as f64,
                    TICKS_PER_SECOND
                ))
                .color(MUTED)
                .small(),
            );
        });

    if !open || cancel {
        *state = None;
        return;
    }

    if apply {
        let position = match parse_field(&dlg.position, rate, start_tc) {
            Ok(t) => t,
            Err(e) => {
                dlg.error = Some(format!("Position: {e}"));
                return;
            }
        };
        let source_in = match parse_field(&dlg.source_in, rate, Tick::ZERO) {
            Ok(t) => t,
            Err(e) => {
                dlg.error = Some(format!("Source In: {e}"));
                return;
            }
        };
        let duration = match parse_field(&dlg.duration, rate, Tick::ZERO) {
            Ok(t) => t,
            Err(e) => {
                dlg.error = Some(format!("Duration: {e}"));
                return;
            }
        };
        if duration.0 <= 0 {
            dlg.error = Some("Duration must be greater than zero".into());
            return;
        }
        let new = ClipTiming {
            start: position,
            duration,
            source_in,
        };
        let ripple = dlg.ripple;
        let seq = dlg.seq;
        let track = dlg.track;
        let clip = dlg.clip;
        match ops_bridge::edit_duration(doc, history, seq, track, clip, new, ripple) {
            Ok(()) => {
                *state = None;
            }
            Err(msg) => {
                if let Some(d) = state.as_mut() {
                    d.error = Some(msg);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_field_accepts_frames_and_tc() {
        let rate = FrameRate::FPS_30;
        let tpf = rate.ticks_per_frame().0;
        assert_eq!(parse_field("90", rate, Tick::ZERO).unwrap(), Tick(90 * tpf));
        assert_eq!(
            parse_field("00:00:03:00", rate, Tick::ZERO).unwrap(),
            Tick(90 * tpf)
        );
        // Sequence start offset: TC label includes start, field stores relative.
        let start = Tick(30 * tpf); // 00:00:01:00
        assert_eq!(
            parse_field("00:00:02:00", rate, start).unwrap(),
            Tick(30 * tpf)
        );
    }
}
