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
/// Returns `true` when the value changed (drag, typed number/expr, or reset).
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
        .speed(speed)
        .custom_parser(move |s| eval_in_range(s, &vars_owned, range_for_parser).ok());
    if let Some((lo, hi)) = range {
        drag = drag.range(lo..=hi);
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
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, f64)]) -> HashMap<String, f64> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), *v))
            .collect()
    }

    #[test]
    fn arithmetic_and_parens() {
        let v = vars(&[]);
        assert_eq!(eval("1+2*3", &v).unwrap(), 7.0);
        assert_eq!(eval("(1+2)*3", &v).unwrap(), 9.0);
        assert_eq!(eval("10/4", &v).unwrap(), 2.5);
        assert_eq!(eval("-3+5", &v).unwrap(), 2.0);
    }

    #[test]
    fn unicode_ops() {
        let v = vars(&[]);
        assert_eq!(eval("6×7", &v).unwrap(), 42.0);
        assert_eq!(eval("8÷2", &v).unwrap(), 4.0);
        assert_eq!(eval("10−3", &v).unwrap(), 7.0);
    }

    #[test]
    fn percent_and_bare_vars() {
        let v = vars(&[("w", 1920.0), ("radius", 10.0)]);
        assert_eq!(eval("%w/2", &v).unwrap(), 960.0);
        assert_eq!(eval("radius*2", &v).unwrap(), 20.0);
        assert_eq!(eval("%radius+1", &v).unwrap(), 11.0);
    }

    #[test]
    fn unknown_var_and_div_zero() {
        let v = vars(&[]);
        assert!(matches!(
            eval("%missing", &v),
            Err(ExprError::UnknownVar(_))
        ));
        assert_eq!(eval("1/0", &v), Err(ExprError::DivByZero));
    }

    #[test]
    fn range_refusal_not_clamp() {
        let v = vars(&[]);
        assert!(matches!(
            eval_in_range("999", &v, Some((0.0, 100.0))),
            Err(ExprError::OutOfRange { value: 999.0, .. })
        ));
        assert_eq!(eval_in_range("50", &v, Some((0.0, 100.0))).unwrap(), 50.0);
    }

    #[test]
    fn vars_from_params_short_and_frame() {
        let m = vars_from_params([("params.radius", 4.0)], Some(1920.0), Some(1080.0));
        assert_eq!(m.get("radius"), Some(&4.0));
        assert_eq!(m.get("params.radius"), Some(&4.0));
        assert_eq!(m.get("w"), Some(&1920.0));
        assert_eq!(m.get("h"), Some(&1080.0));
    }

    #[test]
    fn neutral_default_prefers_zero_in_range() {
        assert_eq!(neutral_float_default(Some((0.0, 1.0))), 0.0);
        assert_eq!(neutral_float_default(Some((1.0, 4.0))), 1.0);
        assert_eq!(neutral_float_default(None), 0.0);
    }
}
