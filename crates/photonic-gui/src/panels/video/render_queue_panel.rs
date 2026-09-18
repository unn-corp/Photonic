//! Render-queue inspector panel (K-F1 / 26 §14).
//!
//! Lists jobs on the shared [`RenderQueue`] with per-job progress and cancel.
//! Jobs are frozen against later edits at submission; this panel only
//! inspects status.

use egui::{Color32, RichText};
use photonic_video::export::{QueueJobStatus, RenderQueue};

const MUTED: Color32 = Color32::from_rgb(0x7A, 0x7A, 0x9A);
const ACCENT: Color32 = Color32::from_rgb(0x6E, 0x56, 0xCF);
const ERROR: Color32 = Color32::from_rgb(0xF8, 0x71, 0x71);

/// Floating "Render Queue" window. `open` is session state on PhotonicApp.
pub(crate) fn draw_render_queue_panel(ctx: &egui::Context, open: &mut bool, queue: &RenderQueue) {
    if !*open {
        return;
    }
    let mut still_open = *open;
    egui::Window::new("Render Queue")
        .id(egui::Id::new("video_render_queue_panel"))
        .open(&mut still_open)
        .default_width(420.0)
        .default_height(280.0)
        .resizable(true)
        .show(ctx, |ui| {
            let jobs = queue.list();
            if jobs.is_empty() {
                ui.label(
                    RichText::new("No export jobs queued.")
                        .color(MUTED)
                        .italics(),
                );
                ui.label(
                    RichText::new(
                        "Multi-format and per-marker exports from File → Export \
                         land here while single-job exports use the engine path.",
                    )
                    .small()
                    .color(MUTED),
                );
                return;
            }
            ui.label(
                RichText::new(format!("{} job(s)", jobs.len()))
                    .small()
                    .color(MUTED),
            );
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                for job in &jobs {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&job.label).strong());
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let can_cancel = matches!(
                                        job.status,
                                        QueueJobStatus::Queued | QueueJobStatus::Running { .. }
                                    );
                                    if ui
                                        .add_enabled(can_cancel, egui::Button::new("Cancel"))
                                        .clicked()
                                    {
                                        queue.cancel(job.id);
                                    }
                                },
                            );
                        });
                        match &job.status {
                            QueueJobStatus::Queued => {
                                ui.label(RichText::new("Queued").color(MUTED).small());
                            }
                            QueueJobStatus::Running { frame, total, fps } => {
                                let p = if *total > 0 {
                                    (*frame as f32 / *total as f32).clamp(0.0, 1.0)
                                } else {
                                    0.0
                                };
                                ui.add(egui::ProgressBar::new(p).show_percentage());
                                ui.label(
                                    RichText::new(format!("Frame {frame}/{total} · {fps:.1} fps"))
                                        .small()
                                        .color(MUTED),
                                );
                                // Keep the panel live while jobs run.
                                ui.ctx().request_repaint();
                            }
                            QueueJobStatus::Done { out_path } => {
                                ui.label(
                                    RichText::new(format!("Done — {}", out_path.display()))
                                        .color(ACCENT)
                                        .small(),
                                );
                            }
                            QueueJobStatus::Failed { message } => {
                                ui.colored_label(ERROR, format!("Failed: {message}"));
                            }
                            QueueJobStatus::Cancelled => {
                                ui.label(RichText::new("Cancelled").color(MUTED).small());
                            }
                        }
                    });
                    ui.add_space(4.0);
                }
            });
        });
    *open = still_open;
}
