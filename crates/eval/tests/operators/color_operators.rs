// `color/luminance` and `color/contrast`: WCAG 2.2 relative luminance and contrast ratio of CSS colours.

use orbital_core_base::json_value::from_json_value;
use orbital_core_base::value::Value;
use orbital_core_eval::evaluator::context::BindingContextBuilder;
use orbital_core_eval::evaluator::Evaluator;

fn run(expr: serde_json::Value) -> Result<Value, String> {
    let ctx = BindingContextBuilder::new().build();
    let sexpr = from_json_value(&expr).unwrap();
    Evaluator::new()
        .evaluate(&sexpr, &ctx)
        .map_err(|e| format!("{e:?}"))
}

fn num(expr: serde_json::Value) -> f64 {
    match run(expr).unwrap() {
        Value::Number(n) => n,
        other => panic!("expected a number, got {other:?}"),
    }
}

fn contrast(a: &str, b: &str) -> f64 {
    num(serde_json::json!(["color/contrast", a, b]))
}

fn round2(n: f64) -> f64 {
    (n * 100.0).round() / 100.0
}

#[test]
fn luminance_of_white_and_black() {
    assert_eq!(num(serde_json::json!(["color/luminance", "#fff"])), 1.0);
    assert_eq!(num(serde_json::json!(["color/luminance", "#000"])), 0.0);
}

#[test]
fn luminance_is_wcag_for_a_mid_grey() {
    let l = num(serde_json::json!(["color/luminance", "#777777"]));
    assert!((l - 0.18447).abs() < 1e-4, "{l}");
}

#[test]
fn black_on_white_is_twenty_one() {
    assert_eq!(contrast("#000", "#fff"), 21.0);
}

#[test]
fn grey_777_on_white_is_4_48() {
    assert_eq!(round2(contrast("#777777", "#ffffff")), 4.48);
}

#[test]
fn same_colour_is_one_and_order_does_not_matter() {
    assert_eq!(contrast("#123456", "#123456"), 1.0);
    assert_eq!(contrast("#fff", "#000"), contrast("#000", "#fff"));
    assert_eq!(
        contrast("#777777", "#ffffff"),
        contrast("#ffffff", "#777777")
    );
}

#[test]
fn every_css_syntax_reads_to_the_same_colour() {
    let white = contrast("#000", "#fff");
    for text in [
        "#ffffff",
        "#ffff",
        "#ffffffff",
        "rgb(255,255,255)",
        "rgba(255, 255, 255, 1)",
        "rgb(255 255 255)",
        "rgb(100% 100% 100%)",
        "hsl(0,0%,100%)",
        "hsla(0, 0%, 100%, 1)",
        "white",
    ] {
        assert_eq!(contrast("#000", text), white, "{text}");
    }
    assert!(contrast("#000", "navy") < 2.0, "navy is dark");
    assert!(contrast("#000", "navy") > 1.0);
}

#[test]
fn translucent_foreground_is_composited_over_the_backdrop() {
    let half = contrast("rgba(0,0,0,0.5)", "#fff");
    let (hi, lo) = (contrast("#7f7f7f", "#fff"), contrast("#808080", "#fff"));
    assert!(lo < half && half < hi, "{lo} < {half} < {hi}");
    assert!((half - contrast("rgb(127.5,127.5,127.5)", "#fff")).abs() < 1e-6);
    assert_eq!(contrast("rgba(0,0,0,1)", "#fff"), 21.0);
    assert_eq!(contrast("transparent", "#fff"), 1.0);
}

#[test]
fn malformed_colours_are_errors() {
    for bad in ["#12", "rgb(1,2)", "not a colour", "", "var(--x)"] {
        assert!(
            run(serde_json::json!(["color/luminance", bad])).is_err(),
            "luminance {bad}"
        );
        assert!(
            run(serde_json::json!(["color/contrast", bad, "#fff"])).is_err(),
            "contrast a {bad}"
        );
        assert!(
            run(serde_json::json!(["color/contrast", "#fff", bad])).is_err(),
            "contrast b {bad}"
        );
    }
    assert!(run(serde_json::json!(["color/luminance", 3])).is_err());
}

#[test]
fn translucent_backdrop_is_refused() {
    let error = run(serde_json::json!([
        "color/contrast",
        "#000",
        "rgba(255,255,255,0.5)"
    ]))
    .expect_err("a translucent backdrop has no luminance");
    assert!(error.contains("opaque"), "{error}");
    assert!(run(serde_json::json!([
        "color/contrast",
        "#000",
        "rgba(255,255,255,1)"
    ]))
    .is_ok());
}

fn composite(top: &str, backdrop: &str) -> Result<Value, String> {
    run(serde_json::json!(["color/composite", top, backdrop]))
}

fn composite_hex(top: &str, backdrop: &str) -> String {
    match composite(top, backdrop).unwrap() {
        Value::String(s) => s.to_string(),
        other => panic!("expected a string, got {other:?}"),
    }
}

#[test]
fn composite_of_half_black_over_white_rounds_half_up_to_808080() {
    assert_eq!(composite_hex("rgba(0,0,0,0.5)", "#fff"), "#808080");
    assert_eq!(composite_hex("rgba(255,255,255,0.5)", "#000"), "#808080");
}

#[test]
fn composite_of_an_opaque_top_returns_itself() {
    assert_eq!(composite_hex("#123456", "#fff"), "#123456");
    assert_eq!(composite_hex("rgb(10 20 30)", "#abcdef"), "#0a141e");
    assert_eq!(composite_hex("transparent", "#abcdef"), "#abcdef");
}

#[test]
fn composite_agrees_with_contrast_on_the_same_pair() {
    let hex = composite_hex("rgba(0,0,0,0.5)", "#fff");
    assert!((contrast("rgba(0,0,0,0.5)", "#fff") - contrast(&hex, "#fff")).abs() < 0.05);
    assert_eq!(composite_hex(&hex, "#fff"), hex);
}

#[test]
fn composite_refuses_a_translucent_backdrop() {
    let error = composite("#000", "rgba(255,255,255,0.5)").expect_err("translucent backdrop");
    assert!(error.contains("opaque"), "{error}");
    assert!(composite("#000", "rgba(255,255,255,1)").is_ok());
}

#[test]
fn composite_refuses_malformed_colours() {
    for bad in ["#12", "rgb(1,2)", "not a colour", "", "var(--x)"] {
        assert!(composite(bad, "#fff").is_err(), "top {bad}");
        assert!(composite("#fff", bad).is_err(), "backdrop {bad}");
    }
    assert!(composite("#fff", "#000").is_ok());
    assert!(run(serde_json::json!(["color/composite", 3, "#fff"])).is_err());
    assert!(run(serde_json::json!(["color/composite", "#fff"])).is_err());
}
