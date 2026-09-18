//! Source marks and explicit audition on the context-driven central monitor.
use crate::panels::PropPanelCtx;
use photonic_core::timeline::{AssetId, AssetSource, Tick};

#[derive(Clone, Copy, Default)]
pub(crate) enum SourceCommand {
    Audition {
        asset: AssetId,
        start: Tick,
        end: Tick,
    },
    #[default]
    Stop,
    Program,
}
const REQUEST: &str = "source_monitor_command";
pub(crate) fn take_command(ctx: &egui::Context) -> Option<(uuid::Uuid, SourceCommand)> {
    ctx.data_mut(|data| data.remove_temp(egui::Id::new(REQUEST)))
}

pub(crate) fn draw_source_monitor(ui: &mut egui::Ui, ctx: &mut PropPanelCtx) {
    ui.heading("Source marks");
    let asset = ctx
        .video
        .source_marks
        .armed_asset
        .and_then(|id| ctx.doc.timeline.as_ref()?.media.assets.get(&id));
    let Some(asset) = asset else {
        ui.label("Select a media-pool asset to mark and audition it here.");
        return;
    };
    let name = match &asset.source {
        AssetSource::File { path, .. } => path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        _ => "Embedded source".into(),
    };
    ui.label(egui::RichText::new(name).strong());
    ctx.video.source_marks.clamp_to_asset(asset);
    let bounds = crate::app::source_marks::source_bounds(asset);
    let fr = asset
        .probe
        .as_ref()
        .and_then(|p| p.video.as_ref())
        .map_or(photonic_core::timeline::FrameRate::FPS_30, |v| v.frame_rate);
    if let Some((start, end)) = bounds.filter(|(start, end)| end > start) {
        let mut seconds = ctx.video.source_marks.source_time.as_seconds_f64();
        if ui
            .add(
                egui::Slider::new(&mut seconds, start.as_seconds_f64()..=end.as_seconds_f64())
                    .text("Source seconds"),
            )
            .changed()
        {
            let time = fr
                .snap(Tick(
                    (seconds * photonic_core::timeline::TICKS_PER_SECOND as f64).round() as i64,
                ))
                .clamp(start, end);
            ctx.video.source_marks.source_time = time;
            *ctx.video.source_monitor_scrub = Some(time);
        }
    } else {
        ui.label("Metadata pending — audition waits for the source duration.");
    }
    ui.horizontal(|ui| {
        let time = ctx.video.source_marks.source_time;
        if ui.button("Mark In · I").clicked() {
            ctx.video.source_marks.set_in(time);
        }
        if ui.button("Mark Out · O").clicked() {
            ctx.video.source_marks.set_out(time);
        }
        if ui.button("Clear").clicked() {
            ctx.video.source_marks.clear_marks();
        }
    });
    let fmt = |tick: Option<Tick>| {
        tick.map(|t| format!("{:.3} s", t.as_seconds_f64()))
            .unwrap_or_else(|| "—".into())
    };
    ui.monospace(format!(
        "In {}  →  Out {}",
        fmt(ctx.video.source_marks.mark_in),
        fmt(ctx.video.source_marks.mark_out)
    ));
    let range = ctx.video.source_marks.audition_range(asset);
    let playing = ctx
        .video
        .source_audition
        .as_ref()
        .is_some_and(|status| status.asset == asset.id && status.playing);
    let mut request = None;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                range.is_some() && !playing,
                egui::Button::new("Audition source audio"),
            )
            .clicked()
        {
            if let Some((start, end)) = range {
                request = Some(SourceCommand::Audition {
                    asset: asset.id,
                    start,
                    end,
                });
            }
        }
        if ui
            .add_enabled(playing, egui::Button::new("Stop audition"))
            .clicked()
        {
            request = Some(SourceCommand::Stop);
        }
    });
    if ui.button("Return to sequence").clicked() {
        request = Some(SourceCommand::Program);
    }
    if let Some(error) = &ctx.video.source_audition_error {
        ui.colored_label(ui.visuals().error_fg_color, error);
    }
    if playing {
        ui.label("Auditioning SOURCE · sequence playhead preserved");
    }
    ui.separator();
    ui.small("Space plays the sequence. I/O set source marks while SOURCE is shown. Comma inserts; period overwrites at the sequence playhead.");
    if let Some(command) = request {
        ui.data_mut(|data| data.insert_temp(egui::Id::new(REQUEST), (ctx.doc.id, command)));
    }
}
