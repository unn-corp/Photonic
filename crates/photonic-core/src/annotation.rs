use crate::node::NodeId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type AnnotationId = Uuid;

/// A non-printing comment or design note attached to a node or the document.
///
/// Annotations are stored in the `.photon` file but stripped from all
/// export formats (SVG, PNG, ICO). They are not part of the undo history —
/// they represent meta-commentary on the design, not visual artwork changes.
///
/// AI agents can leave reasoning annotations explaining *why* a design
/// decision was made; humans can use them for redlines and review comments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Annotation {
    pub id: AnnotationId,
    /// Node this annotation is attached to. `None` means document-level.
    pub node_id: Option<NodeId>,
    /// The comment or note text.
    pub text: String,
    /// Whether this annotation has been resolved/dismissed.
    pub resolved: bool,
    /// Optional author identity (agent name, user name, etc.).
    pub author: Option<String>,
    /// ISO 8601 creation timestamp (e.g. "2026-03-23T14:05:00Z").
    pub created_at: String,
}

impl Annotation {
    pub fn new(node_id: Option<NodeId>, text: impl Into<String>, author: Option<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            node_id,
            text: text.into(),
            resolved: false,
            author,
            created_at: chrono_now(),
        }
    }
}

/// Returns the current UTC time as an ISO 8601 string without pulling in
/// a heavy datetime crate. Falls back to epoch string on any system error.
fn chrono_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => {
            let secs = d.as_secs();
            let (y, mo, day, h, min, s) = epoch_to_ymd(secs);
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                y, mo, day, h, min, s
            )
        }
        Err(_) => "1970-01-01T00:00:00Z".to_string(),
    }
}

/// Convert Unix epoch seconds to (year, month, day, hour, min, sec).
fn epoch_to_ymd(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let s = secs % 60;
    let total_min = secs / 60;
    let min = total_min % 60;
    let total_h = total_min / 60;
    let h = total_h % 24;
    let mut days = total_h / 24;

    // Gregorian calendar arithmetic
    let mut year = 1970u32;
    loop {
        let dy = days_in_year(year);
        if days < dy as u64 {
            break;
        }
        days -= dy as u64;
        year += 1;
    }
    let mut month = 1u32;
    loop {
        let dm = days_in_month(year, month);
        if days < dm as u64 {
            break;
        }
        days -= dm as u64;
        month += 1;
    }
    (year, month, days as u32 + 1, h as u32, min as u32, s as u32)
}

fn is_leap(y: u32) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}
fn days_in_year(y: u32) -> u32 {
    if is_leap(y) {
        366
    } else {
        365
    }
}
fn days_in_month(y: u32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(y) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}
