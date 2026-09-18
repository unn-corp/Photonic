//! ASS (Advanced SubStation Alpha) import/export — the full-fidelity path
//! (06 §7.1/§7.2).
//!
//! Structural parsing (`[Script Info]`/`[V4+ Styles]`/`[Events]` sections,
//! `Format:`/`Style:`/`Dialogue:` lines) plus the specific override-tag
//! subset 06 §7.1's mapping table names: `\k`/`\kf`/`\ko` karaoke duration
//! tags become `CaptionWord` timings; everything else inside a `{...}`
//! override block (`\move`, `\fad`, transforms, drawing-mode `\p`, and any
//! other tag) is dropped and counted for the non-blocking import summary.
//! `\N`/`\n`/`\h` text escapes are normalized to spaces (line breaks are
//! never stored, 06 §3.5) rather than treated as styling directives.

use std::collections::HashMap;

use photonic_core::timeline::{
    CaptionBackground, CaptionCue, CaptionStyle, CaptionTrack, CaptionWord, KaraokeMode, Tick,
    TICKS_PER_SECOND,
};
use photonic_core::Color;

use super::{ExportSummary, ImportSummary, InterchangeError};
use crate::captions::proportional::distribute_caption_words;

const CENTISECOND_TICKS: i64 = TICKS_PER_SECOND / 100;

// ── Timestamps (`H:MM:SS.cc`, centiseconds) ─────────────────────────────────

fn parse_ass_time(s: &str) -> Option<Tick> {
    let s = s.trim();
    let (hms, cs) = s.split_once('.')?;
    let parts: Vec<&str> = hms.split(':').collect();
    let [h, m, sec] = parts.as_slice() else {
        return None;
    };
    let h: i64 = h.parse().ok()?;
    let m: i64 = m.parse().ok()?;
    let sec: i64 = sec.parse().ok()?;
    let cs: i64 = cs.trim().parse().ok()?;
    let total_cs = (h * 3600 + m * 60 + sec) * 100 + cs;
    Some(Tick(CENTISECOND_TICKS * total_cs))
}

fn format_ass_time(t: Tick) -> String {
    let total_cs = t.0.max(0) / CENTISECOND_TICKS;
    let cs = total_cs % 100;
    let total_sec = total_cs / 100;
    let s = total_sec % 60;
    let total_min = total_sec / 60;
    let m = total_min % 60;
    let h = total_min / 60;
    format!("{h}:{m:02}:{s:02}.{cs:02}")
}

fn ticks_to_centiseconds(t: Tick) -> i64 {
    (t.0.max(0)) / CENTISECOND_TICKS
}

fn centiseconds_to_ticks(cs: i64) -> Tick {
    Tick(CENTISECOND_TICKS * cs.max(0))
}

// ── Colors (`&HAABBGGRR&`, ASS alpha inverted: 00 = opaque, FF = transparent) ─

fn parse_ass_color(s: &str) -> Option<Color> {
    let s = s.trim().trim_start_matches("&H").trim_start_matches("&h");
    let hex = s.trim_end_matches('&');
    let value = u32::from_str_radix(hex, 16).ok()?;
    let (aa, bb, gg, rr) = if hex.len() > 6 {
        (
            ((value >> 24) & 0xFF) as u8,
            ((value >> 16) & 0xFF) as u8,
            ((value >> 8) & 0xFF) as u8,
            (value & 0xFF) as u8,
        )
    } else {
        (
            0u8,
            ((value >> 16) & 0xFF) as u8,
            ((value >> 8) & 0xFF) as u8,
            (value & 0xFF) as u8,
        )
    };
    Some(Color {
        r: rr as f32 / 255.0,
        g: gg as f32 / 255.0,
        b: bb as f32 / 255.0,
        a: 1.0 - aa as f32 / 255.0,
    })
}

fn format_ass_color(c: Color) -> String {
    let aa = ((1.0 - c.a).clamp(0.0, 1.0) * 255.0).round() as u8;
    let bb = (c.b.clamp(0.0, 1.0) * 255.0).round() as u8;
    let gg = (c.g.clamp(0.0, 1.0) * 255.0).round() as u8;
    let rr = (c.r.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("&H{aa:02X}{bb:02X}{gg:02X}{rr:02X}")
}

// ── Field-list parsing (`Format:` line defines column order/names) ─────────

fn parse_format_line(v: &str) -> Vec<String> {
    v.split(',').map(|s| s.trim().to_string()).collect()
}

/// Split a `Style:`/`Dialogue:` value list into exactly `field_count` parts,
/// with the last part absorbing any embedded commas (needed for `Dialogue`'s
/// trailing `Text` field).
fn split_fields(v: &str, field_count: usize) -> Vec<String> {
    if field_count == 0 {
        return Vec::new();
    }
    v.splitn(field_count, ',').map(|s| s.to_string()).collect()
}

fn field_map<'a>(names: &'a [String], values: &'a [String]) -> HashMap<&'a str, &'a str> {
    names
        .iter()
        .map(String::as_str)
        .zip(values.iter().map(String::as_str))
        .collect()
}

// ── Style mapping (06 §7.1's table, both directions) ────────────────────────

fn build_caption_style(map: &HashMap<&str, &str>, res_x: f32, res_y: f32) -> CaptionStyle {
    let mut style = CaptionStyle::default();

    if let Some(v) = map.get("Fontname") {
        style.font_family = v.trim().to_string();
    }
    if let Some(v) = map
        .get("Fontsize")
        .and_then(|s| s.trim().parse::<f32>().ok())
    {
        style.font_size = v;
    }
    if let Some(bold) = map.get("Bold").and_then(|s| s.trim().parse::<i32>().ok()) {
        style.weight = if bold != 0 { 700 } else { 400 };
    }
    if let Some(c) = map.get("PrimaryColour").and_then(|s| parse_ass_color(s)) {
        style.fill = c;
    }

    let outline_w = map
        .get("Outline")
        .and_then(|s| s.trim().parse::<f32>().ok())
        .unwrap_or(0.0);
    style.stroke = if outline_w > 0.0 {
        let color = map
            .get("OutlineColour")
            .and_then(|s| parse_ass_color(s))
            .unwrap_or(Color::BLACK);
        Some((color, outline_w))
    } else {
        None
    };

    let border_style = map
        .get("BorderStyle")
        .and_then(|s| s.trim().parse::<i32>().ok())
        .unwrap_or(1);
    style.background = if border_style == 3 {
        let color = map
            .get("BackColour")
            .and_then(|s| parse_ass_color(s))
            .unwrap_or(Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.5,
            });
        let margin_v = map
            .get("MarginV")
            .and_then(|s| s.trim().parse::<f32>().ok())
            .unwrap_or(0.0);
        Some(CaptionBackground {
            color,
            corner_radius: 0.0,
            padding: (margin_v / res_y.max(1.0)).clamp(0.0, 1.0),
        })
    } else {
        None
    };

    let alignment = map
        .get("Alignment")
        .and_then(|s| s.trim().parse::<i32>().ok())
        .unwrap_or(2);
    let margin_l = map
        .get("MarginL")
        .and_then(|s| s.trim().parse::<f32>().ok())
        .unwrap_or(0.0);
    let margin_r = map
        .get("MarginR")
        .and_then(|s| s.trim().parse::<f32>().ok())
        .unwrap_or(0.0);
    let margin_v = map
        .get("MarginV")
        .and_then(|s| s.trim().parse::<f32>().ok())
        .unwrap_or(0.0);
    style.position = alignment_to_position(alignment, margin_l, margin_r, margin_v, res_x, res_y);

    style
}

/// ASS v4+ numpad alignment (1-9) + margins → normalized `[x, y]` position.
fn alignment_to_position(
    alignment: i32,
    margin_l: f32,
    margin_r: f32,
    margin_v: f32,
    res_x: f32,
    res_y: f32,
) -> [f32; 2] {
    let res_x = res_x.max(1.0);
    let res_y = res_y.max(1.0);
    let x = match alignment {
        1 | 4 | 7 => margin_l / res_x,
        3 | 6 | 9 => 1.0 - margin_r / res_x,
        _ => 0.5,
    };
    let y = match alignment {
        7..=9 => margin_v / res_y,
        4..=6 => 0.5,
        _ => 1.0 - margin_v / res_y, // 1, 2, 3: bottom row
    };
    [x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)]
}

/// Inverse of [`alignment_to_position`], best-effort: classifies the
/// position into the nearest of the 9 zones rather than reproducing an
/// arbitrary continuous position exactly (ASS has no continuous position
/// primitive at the style level).
fn position_to_alignment_and_margins(
    position: [f32; 2],
    res_x: f32,
    res_y: f32,
) -> (i32, f32, f32, f32) {
    let [x, y] = position;
    let col = if x < 1.0 / 3.0 {
        0
    } else if x > 2.0 / 3.0 {
        2
    } else {
        1
    };
    let row = if y < 1.0 / 3.0 {
        0
    } else if y > 2.0 / 3.0 {
        2
    } else {
        1
    }; // 0=top,1=mid,2=bottom
       // Numpad layout: row 0 (top) -> 7/8/9, row 1 (mid) -> 4/5/6, row 2 (bottom) -> 1/2/3.
    let alignment = match row {
        0 => 7 + col,
        1 => 4 + col,
        _ => 1 + col,
    };
    let margin_l = if col == 0 { x * res_x } else { 0.0 };
    let margin_r = if col == 2 { (1.0 - x) * res_x } else { 0.0 };
    let margin_v = match row {
        0 => y * res_y,
        2 => (1.0 - y) * res_y,
        _ => 0.0,
    };
    (alignment, margin_l, margin_r, margin_v)
}

const STYLE_FORMAT_FIELDS: &str = "Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding";
const EVENT_FORMAT_FIELDS: &str =
    "Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text";

fn style_to_ass_line(name: &str, style: &CaptionStyle, res: (f32, f32)) -> String {
    let bold = if style.weight >= 700 { -1 } else { 0 };
    let (outline_color, outline_w) = style.stroke.unwrap_or((Color::BLACK, 0.0));
    let border_style = if style.background.is_some() { 3 } else { 1 };
    let back_color = style.background.map(|b| b.color).unwrap_or(Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    });
    let (alignment, margin_l, margin_r, margin_v) =
        position_to_alignment_and_margins(style.position, res.0, res.1);

    format!(
        "Style: {name},{font},{size},{primary},{secondary},{outline_color},{back},{bold},0,0,0,100,100,0,0,{border},{outline},0,{align},{ml},{mr},{mv},1",
        name = name,
        font = style.font_family,
        size = style.font_size,
        primary = format_ass_color(style.fill),
        secondary = format_ass_color(style.fill),
        outline_color = format_ass_color(outline_color),
        back = format_ass_color(back_color),
        bold = bold,
        border = border_style,
        outline = outline_w,
        align = alignment,
        ml = margin_l.round() as i32,
        mr = margin_r.round() as i32,
        mv = margin_v.round() as i32,
    )
}

// ── Dialogue text: override-tag tokenizer + karaoke extraction ─────────────

enum TextSeg {
    Text(String),
    Karaoke(i64),
    OtherTag,
}

/// Split one `{...}` override block into individual `\`-prefixed tags.
fn split_tags(block: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let mut cur = String::new();
    for c in block.chars() {
        if c == '\\' && !cur.is_empty() {
            tags.push(std::mem::take(&mut cur));
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        tags.push(cur);
    }
    tags
}

fn karaoke_duration_cs(tag: &str) -> Option<i64> {
    let t = tag.trim();
    let rest = t
        .strip_prefix("\\kf")
        .or_else(|| t.strip_prefix("\\ko"))
        .or_else(|| t.strip_prefix("\\k"))?;
    rest.trim().parse::<i64>().ok()
}

/// Tokenize `Dialogue` text into text runs, karaoke tags, and other
/// (dropped) tags. `\N`/`\n`/`\h` literal escapes become spaces.
fn tokenize_text(text: &str) -> (Vec<TextSeg>, usize) {
    let mut segs = Vec::new();
    let mut dropped = 0usize;
    let mut buf = String::new();
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '{' {
            let mut block = String::new();
            for c2 in chars.by_ref() {
                if c2 == '}' {
                    break;
                }
                block.push(c2);
            }
            if !buf.is_empty() {
                segs.push(TextSeg::Text(std::mem::take(&mut buf)));
            }
            for tag in split_tags(&block) {
                if tag.trim().is_empty() {
                    continue;
                }
                if let Some(cs) = karaoke_duration_cs(&tag) {
                    segs.push(TextSeg::Karaoke(cs));
                } else {
                    segs.push(TextSeg::OtherTag);
                    dropped += 1;
                }
            }
        } else if c == '\\' && matches!(chars.peek(), Some('N') | Some('n') | Some('h')) {
            chars.next();
            buf.push(' ');
        } else {
            buf.push(c);
        }
    }
    if !buf.is_empty() {
        segs.push(TextSeg::Text(buf));
    }
    (segs, dropped)
}

fn plain_text_from_segs(segs: &[TextSeg]) -> String {
    segs.iter()
        .filter_map(|s| match s {
            TextSeg::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Build `CaptionWord`s for one `Dialogue` line. Returns `(words,
/// used_karaoke, dropped_directive_count)`; when `used_karaoke` is false the
/// caller falls back to proportional distribution across `[start, end]`.
fn words_from_dialogue_text(text: &str, start: Tick, end: Tick) -> (Vec<CaptionWord>, bool, usize) {
    let (segs, dropped) = tokenize_text(text);
    if !segs.iter().any(|s| matches!(s, TextSeg::Karaoke(_))) {
        let plain = plain_text_from_segs(&segs);
        return (distribute_caption_words(&plain, start, end), false, dropped);
    }

    let mut words = Vec::new();
    let mut cursor = start;
    let mut pending: Option<Tick> = None;
    for seg in segs {
        match seg {
            TextSeg::Karaoke(cs) => pending = Some(centiseconds_to_ticks(cs)),
            TextSeg::OtherTag => {}
            TextSeg::Text(t) => {
                let dur = pending.take().unwrap_or(Tick(0));
                let trimmed = t.trim();
                let run_end = cursor.saturating_add(dur);
                if !trimmed.is_empty() {
                    words.extend(distribute_caption_words(trimmed, cursor, run_end));
                }
                cursor = run_end;
            }
        }
    }
    let _ = end; // karaoke cumulative timing is authoritative over the line's nominal End (real-world ASS rounding)
    (words, true, dropped)
}

fn cue_to_karaoke_text(cue: &CaptionCue, mode: KaraokeMode) -> String {
    let mut out = String::new();
    for (i, w) in cue.words.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let dur_cs = ticks_to_centiseconds(w.end.saturating_sub(w.start));
        match mode {
            KaraokeMode::FillSweep => out.push_str(&format!("{{\\kf{dur_cs}}}")),
            KaraokeMode::WordPop => out.push_str(&format!("{{\\k{dur_cs}}}")),
            // ASS has no per-word-only underline primitive — best-effort via
            // a `\u1` override scoped to the tagged run (06 §7.2, flagged in
            // the export summary, not silently perfect).
            KaraokeMode::Underline => out.push_str(&format!("{{\\k{dur_cs}\\u1}}")),
        }
        out.push_str(&w.text);
    }
    out
}

// ── Public API ────────────────────────────────────────────────────────────

pub struct AssImportResult {
    pub track: CaptionTrack,
    pub summary: ImportSummary,
}

pub fn parse_ass(input: &str, track_name: &str) -> Result<AssImportResult, InterchangeError> {
    let normalized = input.replace("\r\n", "\n");

    let mut section = String::new();
    let mut res_x = 384.0f32;
    let mut res_y = 288.0f32;
    let mut style_fields: Vec<String> = Vec::new();
    let mut styles: HashMap<String, CaptionStyle> = HashMap::new();
    let mut event_fields: Vec<String> = Vec::new();
    let mut cues: Vec<CaptionCue> = Vec::new();
    let mut dropped_directives = 0usize;
    let mut any_approximated = false;

    for raw_line in normalized.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line.to_ascii_lowercase();
            continue;
        }

        match section.as_str() {
            "[script info]" => {
                if let Some(v) = line.strip_prefix("PlayResX:") {
                    res_x = v.trim().parse().unwrap_or(res_x);
                } else if let Some(v) = line.strip_prefix("PlayResY:") {
                    res_y = v.trim().parse().unwrap_or(res_y);
                }
            }
            "[v4+ styles]" | "[v4 styles]" => {
                if let Some(v) = line.strip_prefix("Format:") {
                    style_fields = parse_format_line(v);
                } else if let Some(v) = line.strip_prefix("Style:") {
                    let values = split_fields(v, style_fields.len());
                    if values.len() == style_fields.len() {
                        let map = field_map(&style_fields, &values);
                        if let Some(name) = map.get("Name") {
                            styles.insert(
                                name.trim().to_string(),
                                build_caption_style(&map, res_x, res_y),
                            );
                        }
                    }
                }
            }
            "[events]" => {
                if let Some(v) = line.strip_prefix("Format:") {
                    event_fields = parse_format_line(v);
                } else if let Some(v) = line.strip_prefix("Dialogue:") {
                    let values = split_fields(v, event_fields.len());
                    if values.len() != event_fields.len() {
                        continue;
                    }
                    let map = field_map(&event_fields, &values);
                    let (Some(start), Some(end)) = (
                        map.get("Start").and_then(|s| parse_ass_time(s)),
                        map.get("End").and_then(|s| parse_ass_time(s)),
                    ) else {
                        continue;
                    };
                    let style_name = map
                        .get("Style")
                        .copied()
                        .unwrap_or("Default")
                        .trim()
                        .to_string();
                    let text_raw = map.get("Text").copied().unwrap_or("");

                    let (words, used_karaoke, dropped) =
                        words_from_dialogue_text(text_raw, start, end);
                    dropped_directives += dropped;
                    if !used_karaoke {
                        any_approximated = true;
                    }
                    if words.is_empty() {
                        continue;
                    }

                    let mut cue = CaptionCue::new(start, end, words);
                    if let Some(style) = styles.get(&style_name) {
                        cue.style_override = Some(style.clone());
                    }
                    cues.push(cue);
                }
            }
            _ => {}
        }
    }

    let mut track = CaptionTrack::new(track_name);
    if let Some(default_style) = styles.get("Default") {
        track.style = default_style.clone();
    }
    track.cues = cues;

    let mut notes = Vec::new();
    if dropped_directives > 0 {
        notes.push(format!("{dropped_directives} styling directives dropped"));
    }
    let summary = ImportSummary {
        cues_imported: track.cues.len(),
        any_words_approximated: any_approximated,
        notes,
    };

    Ok(AssImportResult { track, summary })
}

pub fn write_ass(track: &CaptionTrack) -> (String, ExportSummary) {
    let res = (1920.0f32, 1080.0f32);
    let mut notes = Vec::new();
    let mut out = String::new();

    out.push_str("[Script Info]\n");
    out.push_str("ScriptType: v4.00+\n");
    out.push_str(&format!("PlayResX: {}\n", res.0 as i32));
    out.push_str(&format!("PlayResY: {}\n", res.1 as i32));

    out.push_str("\n[V4+ Styles]\n");
    out.push_str(&format!("Format: {STYLE_FORMAT_FIELDS}\n"));

    let mut style_names: Vec<(String, CaptionStyle)> =
        vec![("Default".to_string(), track.style.clone())];
    for (i, cue) in track.cues.iter().enumerate() {
        if let Some(style) = &cue.style_override {
            style_names.push((format!("Cue{i}"), style.clone()));
        }
    }
    for (name, style) in &style_names {
        out.push_str(&style_to_ass_line(name, style, res));
        out.push('\n');
        if style.highlight.map(|h| h.mode) == Some(KaraokeMode::Underline) {
            notes.push(format!(
                "style `{name}`: Underline karaoke approximated via \\u1 override (ASS has no per-word-only underline primitive)"
            ));
        }
    }

    out.push_str("\n[Events]\n");
    out.push_str(&format!("Format: {EVENT_FORMAT_FIELDS}\n"));
    for (i, cue) in track.cues.iter().enumerate() {
        let (style_name, style): (String, &CaptionStyle) = match &cue.style_override {
            Some(s) => (format!("Cue{i}"), s),
            None => ("Default".to_string(), &track.style),
        };
        let mode = style
            .highlight
            .map(|h| h.mode)
            .unwrap_or(KaraokeMode::WordPop);
        out.push_str(&format!(
            "Dialogue: 0,{},{},{},,0,0,0,,{}\n",
            format_ass_time(cue.start),
            format_ass_time(cue.end),
            style_name,
            cue_to_karaoke_text(cue, mode)
        ));
    }

    (out, ExportSummary { notes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::{KaraokeStyle, Tick};

    const KARAOKE_SAMPLE: &str = "[Script Info]\nScriptType: v4.00+\nPlayResX: 1920\nPlayResY: 1080\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,48,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,-1,0,0,0,100,100,0,0,1,2,0,2,10,10,10,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.00,0:00:04.00,Default,,0,0,0,,{\\k50}Hello {\\k100}there {\\k75}friend\n";

    #[test]
    fn parses_script_info_and_style() {
        let result = parse_ass(KARAOKE_SAMPLE, "Imported").unwrap();
        assert_eq!(result.track.style.font_family, "Arial");
        assert_eq!(result.track.style.font_size, 48.0);
        assert_eq!(result.track.style.weight, 700); // Bold = -1
        assert_eq!(result.track.style.fill, Color::WHITE);
    }

    #[test]
    fn extracts_karaoke_word_timing_from_k_tags() {
        let result = parse_ass(KARAOKE_SAMPLE, "Imported").unwrap();
        assert_eq!(result.track.cues.len(), 1);
        let words = &result.track.cues[0].words;
        assert_eq!(words.len(), 3);
        assert_eq!(words[0].text, "Hello");
        assert_eq!(words[0].start, Tick::from_seconds(1));
        // 50cs = 500ms
        assert_eq!(
            words[0].end,
            Tick::from_seconds(1) + centiseconds_to_ticks(50)
        );
        assert_eq!(words[1].text, "there");
        assert_eq!(words[1].start, words[0].end);
        assert_eq!(words[2].text, "friend");
        assert!(!result.summary.any_words_approximated);
    }

    #[test]
    fn dialogue_without_karaoke_falls_back_to_proportional() {
        let input = "[Script Info]\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,48,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,10,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,Plain text here\n";
        let result = parse_ass(input, "Imported").unwrap();
        assert!(result.summary.any_words_approximated);
        assert_eq!(result.track.cues[0].text(), "Plain text here");
    }

    #[test]
    fn drops_unsupported_directives_and_reports_count() {
        let input = "[Script Info]\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,48,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,10,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,{\\move(0,0,100,100)\\fad(200,200)}Moving text\n";
        let result = parse_ass(input, "Imported").unwrap();
        assert!(result.summary.notes.iter().any(|n| n.contains("dropped")));
        assert_eq!(result.track.cues[0].text(), "Moving text");
    }

    #[test]
    fn border_style_3_maps_to_background() {
        let input = "[Script Info]\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Boxed,Arial,48,&H00FFFFFF,&H000000FF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,3,2,0,2,10,10,10,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:02.00,Boxed,,0,0,0,,Boxed text\n";
        let result = parse_ass(input, "Imported").unwrap();
        let style = result.track.cues[0].style_override.as_ref().unwrap();
        assert!(style.background.is_some());
    }

    #[test]
    fn karaoke_word_timing_round_trips_through_export_and_import() {
        let imported = parse_ass(KARAOKE_SAMPLE, "Imported").unwrap();
        let (written, _summary) = write_ass(&imported.track);
        let reimported = parse_ass(&written, "Reimported").unwrap();

        assert_eq!(reimported.track.cues.len(), imported.track.cues.len());
        let tolerance = centiseconds_to_ticks(1).0; // rounding to whole centiseconds
        for (a, b) in imported.track.cues[0]
            .words
            .iter()
            .zip(reimported.track.cues[0].words.iter())
        {
            assert_eq!(a.text, b.text);
            assert!((a.start.0 - b.start.0).abs() <= tolerance, "{a:?} vs {b:?}");
            assert!((a.end.0 - b.end.0).abs() <= tolerance, "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn underline_karaoke_mode_is_flagged_in_export_summary() {
        let mut track = CaptionTrack::new("t");
        track.style.highlight = Some(KaraokeStyle {
            mode: KaraokeMode::Underline,
            active_color: Color::WHITE,
            inactive_color: Color::BLACK,
        });
        track.cues.push(CaptionCue::new(
            Tick::ZERO,
            Tick::from_seconds(1),
            vec![CaptionWord::new("hi", Tick::ZERO, Tick::from_seconds(1))],
        ));
        let (written, summary) = write_ass(&track);
        assert!(written.contains("\\u1"));
        assert!(summary.notes.iter().any(|n| n.contains("Underline")));
    }

    #[test]
    fn color_round_trips_through_ass_hex_format() {
        let c = Color {
            r: 1.0,
            g: 0.5,
            b: 0.25,
            a: 0.75,
        };
        let hex = format_ass_color(c);
        let back = parse_ass_color(&hex).unwrap();
        assert!((back.r - c.r).abs() < 0.01);
        assert!((back.g - c.g).abs() < 0.01);
        assert!((back.b - c.b).abs() < 0.01);
        assert!((back.a - c.a).abs() < 0.01);
    }

    #[test]
    fn six_digit_color_without_alpha_defaults_to_opaque() {
        let c = parse_ass_color("&H0000FF&").unwrap(); // BGR: blue=00,green=00,red=FF -> pure red
        assert_eq!(c.r, 1.0);
        assert_eq!(c.g, 0.0);
        assert_eq!(c.b, 0.0);
        assert_eq!(c.a, 1.0);
    }
}
