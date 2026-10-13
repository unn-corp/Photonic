//! K-B6 — parameter-field expressions and middle-click reset.
//!
//! Numeric fields accept arithmetic (`+ − * /` and parentheses) plus named
//! cross-parameter references (`%radius`, `%w`, bare `x`, …). Evaluation
//! **refuses** out-of-range results rather than clamping silently (26 K-B6).
//! Middle-click restores a control's registry / seed default.
//!
//! Pure evaluator + a thin egui `DragValue` helper used by the clip inspector
//! (and reusable from color / keyframe panels).

use egui::Ui;
use std::collections::HashMap;

/// Why an expression could not produce a usable number.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprError {
    Empty,
    Syntax,
    UnknownVar(String),
    DivByZero,
    /// Value evaluated fine but sits outside the registry range — refuse.
    OutOfRange {
        value: f64,
        lo: f64,
        hi: f64,
    },
}

/// Evaluate a parameter expression against `vars`.
///
/// Grammar (recursive descent):
/// ```text
/// expr   := term (('+'|'-') term)*
/// term   := factor (('*'|'/') factor)*
/// factor := unary | primary
/// unary  := ('+'|'-') factor
/// primary:= number | '%' ident | ident | '(' expr ')'
/// ```
///
/// Unicode `× ÷ −` are accepted as `* / -`. Identifiers are case-insensitive
/// and match keys in `vars` (callers typically seed short param names + `%w`/`%h`).
pub fn eval(input: &str, vars: &HashMap<String, f64>) -> Result<f64, ExprError> {
    let s = input.trim();
    if s.is_empty() {
        return Err(ExprError::Empty);
    }
    let mut p = Parser {
        src: s.as_bytes(),
        i: 0,
        vars,
    };
    let v = p.parse_expr()?;
    p.skip_ws();
    if p.i != p.src.len() {
        return Err(ExprError::Syntax);
    }
    if !v.is_finite() {
        return Err(ExprError::Syntax);
    }
    Ok(v)
}

/// Like [`eval`], then refuse values outside `range` (inclusive) when present.
pub fn eval_in_range(
    input: &str,
    vars: &HashMap<String, f64>,
    range: Option<(f64, f64)>,
) -> Result<f64, ExprError> {
    let v = eval(input, vars)?;
    if let Some((lo, hi)) = range {
        if v < lo || v > hi {
            return Err(ExprError::OutOfRange { value: v, lo, hi });
        }
    }
    Ok(v)
}

/// Seed a variable map from float params keyed by full path (`params.radius`)
/// and short name (`radius`). Optional sequence frame size as `w`/`h`.
pub fn vars_from_params<'a, I>(
    params: I,
    frame_w: Option<f64>,
    frame_h: Option<f64>,
) -> HashMap<String, f64>
where
    I: IntoIterator<Item = (&'a str, f64)>,
{
    let mut m = HashMap::new();
    for (path, v) in params {
        let key = path.to_ascii_lowercase();
        m.insert(key.clone(), v);
        if let Some(short) = path.rsplit('.').next() {
            m.insert(short.to_ascii_lowercase(), v);
        }
        // Strip a leading `params.` for bare-path lookup.
        if let Some(rest) = path.strip_prefix("params.") {
            m.insert(rest.to_ascii_lowercase(), v);
        }
    }
    if let Some(w) = frame_w {
        m.insert("w".into(), w);
    }
    if let Some(h) = frame_h {
        m.insert("h".into(), h);
    }
    m
}

/// Registry-neutral float default (mirrors `EffectParams::seed`): 0 if in range,
/// else the range minimum.
pub fn neutral_float_default(range: Option<(f64, f64)>) -> f64 {
    match range {
        Some((lo, hi)) if (lo..=hi).contains(&0.0) => 0.0,
        Some((lo, _)) => lo,
        None => 0.0,
    }
}

/// DragValue with expression parsing + middle-click reset to `default`.
///
/// Returns `true` when the value changed (drag, committed number/expr, or reset).
/// Text stays a draft until Enter/focus loss; Escape cancels it.
/// Typed expressions outside `range` are refused (parser returns `None`); mouse
/// drag still uses egui's inclusive range clamp for continuous scrubbing.
pub fn float_drag(
    ui: &mut Ui,
    value: &mut f64,
    default: f64,
    range: Option<(f64, f64)>,
    vars: &HashMap<String, f64>,
    speed: f64,
) -> bool {
    let vars_owned = vars.clone();
    let range_for_parser = range;
    let mut drag = egui::DragValue::new(value)
        // A typed expression is one edit, not one command per valid prefix.
        // Enter/focus loss commits; Escape leaves the authored value unchanged.
        // Mouse scrubbing still updates continuously under pointer coalescing.
        .update_while_editing(false)
        .speed(speed)
        .custom_parser(move |s| eval_in_range(s, &vars_owned, range_for_parser).ok());
    if let Some((lo, hi)) = range {
        drag = drag.range(lo..=hi).clamp_existing_to_range(false);
    }
    let resp = ui
        .add(drag)
        .on_hover_text("Type arithmetic (e.g. 10+5, %w/2). Middle-click resets to default.");
    let mut changed = resp.changed();
    if resp.middle_clicked() && (*value - default).abs() > f64::EPSILON {
        *value = default;
        changed = true;
    }
    changed
}

/// `f32` facade for grade / CDL editors that store single-precision params.
pub fn float_drag_f32(
    ui: &mut Ui,
    value: &mut f32,
    default: f32,
    range: Option<(f64, f64)>,
    vars: &HashMap<String, f64>,
    speed: f64,
) -> bool {
    let mut v = f64::from(*value);
    let changed = float_drag(ui, &mut v, f64::from(default), range, vars, speed);
    if changed {
        *value = v as f32;
    }
    changed
}

// ── Parser ───────────────────────────────────────────────────────────────────

struct Parser<'a> {
    src: &'a [u8],
    i: usize,
    vars: &'a HashMap<String, f64>,
}

impl Parser<'_> {
    fn skip_ws(&mut self) {
        while self.i < self.src.len() && self.src[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.i).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.i += 1;
        Some(c)
    }

    fn parse_expr(&mut self) -> Result<f64, ExprError> {
        let mut v = self.parse_term()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b'+') => {
                    self.bump();
                    v += self.parse_term()?;
                }
                Some(b'-') | Some(0xE2) => {
                    // ASCII '-' or start of UTF-8 '−' (U+2212 = e2 88 92)
                    if self.try_minus() {
                        v -= self.parse_term()?;
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
        Ok(v)
    }

    fn parse_term(&mut self) -> Result<f64, ExprError> {
        let mut v = self.parse_factor()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b'*') => {
                    self.bump();
                    v *= self.parse_factor()?;
                }
                Some(b'/') => {
                    self.bump();
                    let d = self.parse_factor()?;
                    if d == 0.0 {
                        return Err(ExprError::DivByZero);
                    }
                    v /= d;
                }
                Some(0xC3) => {
                    // UTF-8 × (c3 97) or ÷ (c3 b7) share the c3 prefix.
                    if self.try_mul() {
                        v *= self.parse_factor()?;
                    } else if self.try_div() {
                        let d = self.parse_factor()?;
                        if d == 0.0 {
                            return Err(ExprError::DivByZero);
                        }
                        v /= d;
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
        Ok(v)
    }

    fn parse_factor(&mut self) -> Result<f64, ExprError> {
        self.skip_ws();
        match self.peek() {
            Some(b'+') => {
                self.bump();
                self.parse_factor()
            }
            Some(b'-') => {
                self.bump();
                Ok(-self.parse_factor()?)
            }
            Some(0xE2) if self.try_minus() => Ok(-self.parse_factor()?),
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Result<f64, ExprError> {
        self.skip_ws();
        match self.peek() {
            Some(b'(') => {
                self.bump();
                let v = self.parse_expr()?;
                self.skip_ws();
                if self.bump() != Some(b')') {
                    return Err(ExprError::Syntax);
                }
                Ok(v)
            }
            Some(b'%') => {
                self.bump();
                self.parse_ident_value()
            }
            Some(c) if c.is_ascii_digit() || c == b'.' => self.parse_number(),
            Some(c) if c.is_ascii_alphabetic() || c == b'_' => self.parse_ident_value(),
            _ => Err(ExprError::Syntax),
        }
    }

    fn parse_number(&mut self) -> Result<f64, ExprError> {
        let start = self.i;
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.bump();
        }
        if self.peek() == Some(b'.') {
            self.bump();
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.bump();
            }
        }
        // Optional scientific exponent.
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            self.bump();
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.bump();
            }
            let exp_start = self.i;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.bump();
            }
            if self.i == exp_start {
                return Err(ExprError::Syntax);
            }
        }
        let s = std::str::from_utf8(&self.src[start..self.i]).map_err(|_| ExprError::Syntax)?;
        s.parse::<f64>().map_err(|_| ExprError::Syntax)
    }

    fn parse_ident_value(&mut self) -> Result<f64, ExprError> {
        let start = self.i;
        while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'_') {
            self.bump();
        }
        if self.i == start {
            return Err(ExprError::Syntax);
        }
        let name = std::str::from_utf8(&self.src[start..self.i])
            .map_err(|_| ExprError::Syntax)?
            .to_ascii_lowercase();
        self.vars
            .get(&name)
            .copied()
            .ok_or(ExprError::UnknownVar(name))
    }

    /// Consume UTF-8 '−' (U+2212) or ASCII '-'. Returns true if consumed.
    fn try_minus(&mut self) -> bool {
        if self.peek() == Some(b'-') {
            self.bump();
            return true;
        }
        // U+2212 MINUS SIGN: e2 88 92
        if self.src.get(self.i..self.i + 3) == Some(&[0xE2, 0x88, 0x92]) {
            self.i += 3;
            return true;
        }
        false
    }

    fn try_mul(&mut self) -> bool {
        // U+00D7 MULTIPLICATION SIGN: c3 97
        if self.src.get(self.i..self.i + 2) == Some(&[0xC3, 0x97]) {
            self.i += 2;
            return true;
        }
        false
    }

    fn try_div(&mut self) -> bool {
        // U+00F7 DIVISION SIGN: c3 b7
        if self.src.get(self.i..self.i + 2) == Some(&[0xC3, 0xB7]) {
            self.i += 2;
            return true;
        }
        false
    }
}

#[cfg(test)]
#[path = "param_expr_tests/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "param_expr_tests/numeric_commit_tests.rs"]
mod numeric_commit_tests;

#[derive(Clone)]
struct NumericSliderDraft {
    original: f64,
    candidate: f64,
    text: Option<String>,
}

/// Slider adapter for a single authored numeric value. Styling and range
/// semantics remain with egui; text-commit behavior is qualified separately.
pub fn numeric_slider<Num: egui::emath::Numeric>(
    ui: &mut Ui,
    value: &mut Num,
    range: std::ops::RangeInclusive<Num>,
    configure: impl for<'a> FnOnce(egui::Slider<'a>) -> egui::Slider<'a>,
) -> egui::Response {
    let draft_id = ui.next_auto_id().with("photonic_numeric_slider_draft");
    let original = *value;
    let mut proposed = original;
    let mut response = ui.add(configure(egui::Slider::new(&mut proposed, range)));
    // Slider forwards its numeric editor ID while that editor has focus.
    // A focused handle has no TextEdit state and keeps live keyboard nudging.
    if !ui.is_enabled() {
        ui.data_mut(|data| {
            data.remove::<NumericSliderDraft>(draft_id);
            data.remove::<String>(response.id);
        });
        response.changed = false;
        return response;
    }
    let text_active =
        response.has_focus() && egui::TextEdit::load_state(ui.ctx(), response.id).is_some();
    let cancelled = ui.input(|input| input.key_pressed(egui::Key::Escape));
    let mut draft = ui.data(|data| data.get_temp::<NumericSliderDraft>(draft_id));
    // Pinned egui DragValue retains its editor string under the forwarded ID.
    // Compare it only to detect textual changes: invalid text must reset to
    // the original value, rather than accidentally committing a valid prefix.
    // Event regressions pin this adapter contract for dependency upgrades.
    let text = ui.data(|data| data.get_temp::<String>(response.id));
    if let Some(old) = &draft {
        if old.original != original.to_f64() {
            ui.data_mut(|data| {
                data.remove::<NumericSliderDraft>(draft_id);
                data.remove::<String>(response.id);
            });
            ui.memory_mut(|memory| memory.surrender_focus(response.id));
            response.changed = false;
            return response;
        }
    }
    if text_active {
        if draft.as_ref().is_none_or(|old| old.text != text) {
            draft = Some(NumericSliderDraft {
                original: original.to_f64(),
                candidate: proposed.to_f64(),
                text,
            });
        }
        if let Some(draft) = draft {
            ui.data_mut(|data| data.insert_temp(draft_id, draft));
        }
    } else {
        if !cancelled {
            *value = if response.lost_focus() {
                draft.map_or(proposed, |draft| {
                    if draft.text != text {
                        proposed
                    } else {
                        Num::from_f64(draft.candidate)
                    }
                })
            } else {
                proposed
            };
        }
        let _ = ui.data_mut(|data| data.remove::<NumericSliderDraft>(draft_id));
    }
    response.changed = *value != original;
    response
}

#[cfg(test)]
#[path = "param_expr_tests/slider_commit_tests.rs"]
mod slider_commit_tests;
