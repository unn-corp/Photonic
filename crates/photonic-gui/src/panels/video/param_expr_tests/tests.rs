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
