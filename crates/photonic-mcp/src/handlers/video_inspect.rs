//! Bounded, sequence-scoped state for agents. Pagination pins a document
//! revision so an edit cannot silently shift objects between pages.
use crate::{protocol::ToolResult, server::AppState};
use photonic_core::timeline::{ClipSource, SequenceId, TrackId, TICKS_PER_SECOND};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashSet};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmptyArgs {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetTimelineSnapshotArgs {
    pub sequence_id: SequenceId,
    #[serde(default)]
    pub track_ids: Vec<TrackId>,
    #[serde(default)]
    pub start_ticks: Option<i64>,
    #[serde(default)]
    pub end_ticks: Option<i64>,
    #[serde(default)]
    pub offset: usize,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub expected_revision: Option<u64>,
    #[serde(default)]
    pub include_effects: bool,
}

pub async fn get_timeline_snapshot(state: &AppState, args: GetTimelineSnapshotArgs) -> ToolResult {
    let limit = args.limit.unwrap_or(200);
    if !(1..=1000).contains(&limit)
        || args.track_ids.len() > 128
        || args.start_ticks.is_some_and(|t| t < 0)
        || args.end_ticks.is_some_and(|t| t <= 0)
        || args
            .end_ticks
            .zip(args.start_ticks)
            .is_some_and(|(end, start)| end <= start)
    {
        return ToolResult::error_with_code(
            "InvalidArguments",
            "Use a positive range, 1–1000 clips per page, and at most 128 selected tracks",
        );
    }
    if args.offset > 0 && args.expected_revision.is_none() {
        return ToolResult::error_with_code(
            "RevisionRequired",
            "Continuation pages require the first page's expected_revision",
        );
    }
    let document = state.document.lock().await;
    let revision = state.history.lock().await.revision();
    if args
        .expected_revision
        .is_some_and(|expected| expected != revision)
    {
        return ToolResult::error_with_code(
            "RevisionConflict",
            "Document changed between pages; restart from offset 0",
        )
        .with_data(json!({"actual_revision":revision,"expected_revision":args.expected_revision}));
    }
    let Some(project) = &document.timeline else {
        return ToolResult::error_with_code("NoProject", "No timeline project");
    };
    let Some(sequence) = project.sequences.get(&args.sequence_id) else {
        return ToolResult::error_with_code("NoSequence", "Requested sequence does not exist");
    };
    let selected: HashSet<_> = args.track_ids.iter().copied().collect();
    if selected.iter().any(|id| sequence.track(*id).is_none()) {
        return ToolResult::error_with_code(
            "NoTrack",
            "A selected track does not belong to this sequence",
        );
    }
    let tracks: Vec<_> = sequence
        .tracks()
        .filter(|track| selected.is_empty() || selected.contains(&track.id))
        .collect();
    if tracks.len() > 2000 {
        return ToolResult::error_with_code(
            "ScopeTooLarge",
            "Select track_ids for sequences with more than 2000 tracks",
        );
    }
    let mut clips: Vec<_> = tracks
        .iter()
        .enumerate()
        .flat_map(|(index, track)| track.clips.iter().map(move |clip| (index, *track, clip)))
        .filter(|(_, _, clip)| {
            args.start_ticks.is_none_or(|start| clip.end().0 > start)
                && args.end_ticks.is_none_or(|end| clip.start.0 < end)
        })
        .collect();
    clips.sort_by_key(|(index, _, clip)| (*index, clip.start, clip.id));
    let total = clips.len();
    let start = args.offset.min(total);
    let end = start.saturating_add(limit).min(total);
    let mut asset_ids = BTreeSet::new();
    let clips: Vec<_> = clips[start..end].iter().map(|(_,track,clip)| {
        let asset_id = match clip.source { ClipSource::Asset{asset}|ClipSource::Vector{asset} => Some(asset), _ => None };
        if let Some(asset) = asset_id { asset_ids.insert(asset); }
        let (min_delta,max_delta) = clip.speed.source_delta_range(clip.duration);
        let mut item = json!({
            "sequence_id":sequence.id,"track_id":track.id,"clip_id":clip.id,"asset_id":asset_id,"name":clip.name,
            "start_ticks":clip.start.0,"end_ticks":clip.end().0,"duration_ticks":clip.duration.0,
            "source":clip.source,"source_in_ticks":clip.source_in.0,
            "source_end_ticks":clip.source_in.0.saturating_add(clip.speed.source_delta(clip.duration).0),
            "source_extent_ticks":[clip.source_in.0 as f64+min_delta,clip.source_in.0 as f64+max_delta],
            "speed":clip.speed,"enabled":clip.enabled,"link_group":clip.link_group,"group":clip.group,
            "effect_count":clip.effects.len(),"has_grade":clip.grade.is_some(),"has_audio":clip.audio.is_some(),
        });
        if args.include_effects { item["effects"] = json!(clip.effects); item["grade"] = json!(clip.grade); item["transform"] = json!(clip.transform); }
        item
    }).collect();
    let tracks: Vec<_> = tracks.iter().map(|track| json!({"track_id":track.id,"name":track.name,"kind":track.kind,"enabled":track.enabled,"locked":track.locked,"sync_lock":track.sync_lock,"clip_count":track.clips.len()})).collect();
    let assets: Vec<_> = asset_ids.iter().filter_map(|id| project.media.assets.get(id)).map(|asset| json!({"asset_id":asset.id,"kind":asset.kind,"source":asset.source,"probe":asset.probe,"proxy":asset.proxy,"content_hash":asset.content_hash})).collect();
    let captions: Vec<_> = sequence.caption_tracks.iter().map(|track| json!({"caption_track_id":track.id,"name":track.name,"cue_count":track.cues.len()})).collect();
    let data = json!({
        "revision":revision,"sequence_id":sequence.id,"name":sequence.name,"frame_rate":sequence.frame_rate,
        "ticks_per_second":TICKS_PER_SECOND,"formats":sequence.formats,"active_format":sequence.active_format,
        "work_range":sequence.work_range,"preview_zones":sequence.preview_zones,
        "tracks":tracks,"caption_tracks":captions,"clips":clips,"assets":assets,
        "offset":start,"limit":limit,"total_clips":total,
        "next_page":(end < total).then(||json!({"offset":end,"expected_revision":revision})),
        "source_time_mapping":"source_in_ticks + integral(speed, 0..timeline_tick-start_ticks); end is exclusive; extent includes reversals",
    });
    if data.to_string().len() > 8 * 1024 * 1024 {
        return ToolResult::error_with_code(
            "SnapshotTooLarge",
            "Reduce limit or omit include_effects; snapshots are limited to 8 MiB",
        );
    }
    ToolResult::text("Timeline snapshot").with_data(data)
}

pub fn tool_schemas() -> Vec<Value> {
    vec![crate::schema_gen::tool_definition("get_video_capabilities",
        "Read implemented video editing, transcript, inspection and export capabilities and limits. GPU initialization is null until first render/status call; false indicates failed initialization. FFmpeg availability is probed locally. Obtain full schemas with get_action_schema.",
        json!({"type":"object","properties":{},"additionalProperties":false}),
        json!({"type":"object","properties":{"contract_version":{"type":"integer"},"engine_initialized":{"type":["boolean","null"]},"ffmpeg_available":{"type":"boolean"},"editing":{"type":"object"},"inspection":{"type":"object"},"transcript":{"type":"object"},"export":{"type":"object"},"jobs":{"type":"object"},"discovery":{"type":"object"}},"required":["contract_version","engine_initialized","ffmpeg_available","editing","inspection","transcript","export","jobs","discovery"]}),crate::schema_gen::ToolBehavior::ExternalRead),crate::schema_gen::tool_definition("render_frames_at",
        "Render 1–12 ticks from one sequence/revision as a contact sheet or individual PNGs. GPU downscale limits transfer cost; frames carry time, revision, quality and tile rectangles. Rejects edits during the batch; max 16 MiB of encoded images. Full quality processes originals before reducing for inspection.",
        json!({"type":"object","properties":{
            "sequence_id":{"type":"string","format":"uuid"},"at_ticks":{"type":"array","minItems":1,"maxItems":12,"items":{"type":"integer","minimum":0}},
            "format_index":{"type":"integer","minimum":0},"quality":{"type":"string","enum":["preview","full"],"default":"preview"},
            "max_long_edge":{"type":"integer","minimum":32,"maximum":1024,"default":640},"expected_revision":{"type":"integer","minimum":0},
            "contact_sheet":{"type":"boolean","default":true},"columns":{"type":"integer","minimum":1,"maximum":4,"default":4}
        },"required":["sequence_id","at_ticks"],"additionalProperties":false}),
        json!({"type":"object","properties":{"sequence_id":{"type":"string"},"revision":{"type":"integer"},"format_index":{"type":"integer"},"frames":{"type":"array"},"contact_sheet":{"type":"boolean"},"sheet_size":{"type":["object","null"]}},"required":["sequence_id","revision","format_index","frames","contact_sheet","sheet_size"]}),
        crate::schema_gen::ToolBehavior::ExternalMutation), crate::schema_gen::tool_definition("get_timeline_snapshot",
        "Read a bounded, stably ordered sequence snapshot with revision, tracks, clip/source timing, asset IDs, formats and preview zones. Use next_page offset+expected_revision with the same scope. Rejects stale pagination. Pass revision to apply_video_edit_plan. Source end may precede start for reverse speed; source_extent covers ramps.",
        json!({"type":"object","properties":{
            "sequence_id":{"type":"string","format":"uuid"},"track_ids":{"type":"array","maxItems":128,"items":{"type":"string","format":"uuid"}},
            "start_ticks":{"type":"integer","minimum":0},"end_ticks":{"type":"integer","minimum":1},"offset":{"type":"integer","minimum":0,"default":0},
            "limit":{"type":"integer","minimum":1,"maximum":1000,"default":200},"expected_revision":{"type":"integer","minimum":0},"include_effects":{"type":"boolean","default":false}
        },"required":["sequence_id"],"additionalProperties":false}),
        json!({"type":"object","properties":{
            "revision":{"type":"integer"},"sequence_id":{"type":"string"},"name":{"type":"string"},"frame_rate":{"type":"object"},"ticks_per_second":{"type":"integer"},
            "formats":{"type":"array"},"active_format":{"type":"integer"},"work_range":{"type":["array","null"]},"preview_zones":{"type":"array"},
            "tracks":{"type":"array"},"caption_tracks":{"type":"array"},"clips":{"type":"array"},"assets":{"type":"array"},
            "offset":{"type":"integer"},"limit":{"type":"integer"},"total_clips":{"type":"integer"},"next_page":{"type":["object","null"]},"source_time_mapping":{"type":"string"}
        },"required":["revision","sequence_id","name","frame_rate","ticks_per_second","formats","active_format","work_range","preview_zones","tracks","caption_tracks","clips","assets","offset","limit","total_clips","next_page","source_time_mapping"]}),
        crate::schema_gen::ToolBehavior::ReadOnly)]
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderFramesAtArgs {
    pub sequence_id: SequenceId,
    pub at_ticks: Vec<i64>,
    #[serde(default)]
    pub format_index: Option<usize>,
    #[serde(default = "preview_quality")]
    pub quality: crate::protocol::RenderQualityArg,
    #[serde(default)]
    pub max_long_edge: Option<u32>,
    #[serde(default)]
    pub expected_revision: Option<u64>,
    #[serde(default = "contact_sheet_default")]
    pub contact_sheet: bool,
    #[serde(default)]
    pub columns: Option<usize>,
}
fn preview_quality() -> crate::protocol::RenderQualityArg {
    crate::protocol::RenderQualityArg::Preview
}
fn contact_sheet_default() -> bool {
    true
}

pub async fn render_frames_at(state: &AppState, args: RenderFramesAtArgs) -> ToolResult {
    use base64::Engine as _;
    if args.at_ticks.is_empty()
        || args.at_ticks.len() > 12
        || args.at_ticks.iter().any(|tick| *tick < 0)
    {
        return ToolResult::error_with_code(
            "InvalidArguments",
            "Inspect 1–12 nonnegative ticks per batch",
        );
    }
    let edge = args.max_long_edge.unwrap_or(640);
    let columns = args.columns.unwrap_or(4);
    if !(32..=1024).contains(&edge) || !(1..=4).contains(&columns) {
        return ToolResult::error_with_code(
            "InvalidArguments",
            "max_long_edge must be 32–1024 and columns 1–4",
        );
    }
    let (revision, width, height, format_index) = {
        let document = state.document.lock().await;
        let revision = state.history.lock().await.revision();
        let Some(sequence) = document
            .timeline
            .as_ref()
            .and_then(|p| p.sequences.get(&args.sequence_id))
        else {
            return ToolResult::error_with_code("NoSequence", "Requested sequence does not exist");
        };
        let index = args.format_index.unwrap_or(sequence.active_format);
        let Some(format) = sequence.formats.get(index) else {
            return ToolResult::error_with_code("InvalidArguments", "format_index is out of range");
        };
        (revision, format.width.max(1), format.height.max(1), index)
    };
    if args
        .expected_revision
        .is_some_and(|expected| expected != revision)
    {
        return ToolResult::error_with_code(
            "RevisionConflict",
            "Document changed before visual inspection",
        )
        .with_data(json!({"actual_revision":revision}));
    }
    let scale = (f64::from(edge) / f64::from(width.max(height))).min(1.0);
    let mut images = Vec::new();
    let mut metadata = Vec::new();
    let mut encoded_bytes = 0;
    for (index, tick) in args.at_ticks.iter().enumerate() {
        let result = super::video::render_frame_at(
            state,
            crate::protocol::RenderFrameAtArgs {
                sequence_id: args.sequence_id,
                at_ticks: Some(*tick),
                at_tc: None,
                at_seconds: None,
                format_index: Some(format_index),
                quality: args.quality,
                scale: Some(scale),
                output_format: Some(crate::protocol::RenderOutputFormatArg::Png),
            },
        )
        .await;
        if result.is_error == Some(true) {
            return ToolResult::error_with_code(
                "FrameBatchFailed",
                format!("Frame {index} failed"),
            )
            .with_data(json!({"frame_index":index,"cause":result.structured_content}));
        }
        let mut data = result.structured_content.unwrap_or_default();
        if data["revision"] != revision {
            return ToolResult::error_with_code(
                "RevisionConflict",
                "Document changed during visual inspection; retry the batch",
            )
            .with_data(json!({"actual_revision":data["revision"],"expected_revision":revision}));
        }
        let encoded = result
            .content
            .into_iter()
            .find_map(|content| match content {
                crate::protocol::ContentItem::Image { data, .. } => Some(data),
                _ => None,
            });
        let Some(encoded) = encoded else {
            return ToolResult::error_with_code("FrameBatchFailed", "Renderer produced no image");
        };
        encoded_bytes += encoded.len();
        if encoded_bytes > 16 * 1024 * 1024 {
            return ToolResult::error_with_code(
                "FrameBatchTooLarge",
                "Reduce max_long_edge or the frame count; image batches are limited to 16 MiB",
            );
        }
        data["index"] = json!(index);
        data["requested_tick"] = json!(tick);
        images.push(encoded);
        metadata.push(data);
    }
    if state.history.lock().await.revision() != revision {
        return ToolResult::error_with_code(
            "RevisionConflict",
            "Document changed during visual inspection; retry the batch",
        );
    }
    let mut result = ToolResult::text("Rendered sequence inspection frames");
    let mut sheet_size = None;
    if args.contact_sheet {
        let tile_width = metadata[0]["width"].as_u64().unwrap_or(1) as u32;
        let tile_height = metadata[0]["height"].as_u64().unwrap_or(1) as u32;
        let columns = columns.min(images.len());
        let rows = images.len().div_ceil(columns);
        let sheet_width = tile_width * columns as u32;
        let sheet_height = tile_height * rows as u32;
        let mut sheet = image::RgbaImage::new(sheet_width, sheet_height);
        for (index, encoded) in images.into_iter().enumerate() {
            let bytes = match base64::engine::general_purpose::STANDARD.decode(encoded) {
                Ok(bytes) => bytes,
                Err(error) => return ToolResult::error(error.to_string()),
            };
            let tile = match image::load_from_memory(&bytes) {
                Ok(image) => image.to_rgba8(),
                Err(error) => return ToolResult::error(error.to_string()),
            };
            let x = (index % columns) as u32 * tile_width;
            let y = (index / columns) as u32 * tile_height;
            image::imageops::replace(&mut sheet, &tile, i64::from(x), i64::from(y));
            metadata[index]["sheet_rect"] =
                json!({"x":x,"y":y,"width":tile_width,"height":tile_height});
        }
        let mut bytes = Vec::new();
        if let Err(error) = sheet.write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        ) {
            return ToolResult::error(error.to_string());
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        if encoded.len() > 16 * 1024 * 1024 {
            return ToolResult::error_with_code(
                "FrameBatchTooLarge",
                "Contact sheet exceeds 16 MiB",
            );
        }
        result = result.with_image(encoded);
        sheet_size = Some(json!({"width":sheet_width,"height":sheet_height}));
    } else {
        for image in images {
            result = result.with_image(image);
        }
    }
    result.with_data(json!({"sequence_id":args.sequence_id,"revision":revision,"format_index":format_index,"frames":metadata,"contact_sheet":args.contact_sheet,"sheet_size":sheet_size}))
}

pub async fn get_video_capabilities(state: &AppState) -> ToolResult {
    let ffmpeg = photonic_video::media::ffmpeg_locate::locate().is_ok();
    ToolResult::text("Video editor capabilities").with_data(json!({
        "contract_version":1,"engine_initialized":state.video_engine.initialization_state(),
        "ffmpeg_available":ffmpeg,
        "editing":{"atomic_plans":true,"dry_run":true,"revision_preconditions":true,"retry_receipts":true,"max_operations":super::video_edits::MAX_OPERATIONS,"plan_tools":super::video_edits::PLAN_TOOLS},
        "inspection":{"timeline_snapshot":true,"max_clips_per_page":1000,"frame_batch":true,"contact_sheet":true,"max_frames_per_batch":12,"max_thumbnail_long_edge":1024,"full_source_processing":true,"raw_rgba16f":true},
        "transcript":{"word_edit":true,"range_lift":true,"range_ripple":true,"filler_preview":true,"filler_removal":true},
        "export":{"two_pass":false,"bounded_pipeline":true,"shared_multi_output":true,"max_outputs_per_job":8,"requires_ffmpeg":true},
        "previews":{"chunked_playback":true,"zone_edits":true,"max_zones":128,"default_quality":"full","default_codec":"intra_h264","export_reuse":false},
        "source":{"audition":true,"requires_probe":true,"audio_requires_output_device":true},
        "jobs":{"max_active":super::video_jobs::MAX_ACTIVE_JOBS,"capacity_error":"JobCapacityExceeded"},
        "discovery":{"exact_schema_tool":"get_action_schema","search_tool":"search_actions"}
    }))
}
