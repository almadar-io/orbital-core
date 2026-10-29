//! `prob/*`: distribution sampling, rejection-sampling inference and sample statistics.
//!
//! The stream is Mulberry32, the generator `@almadar/evaluator` ran, so a `prob/seed` makes a program replay the
//! same numbers on both paths. Its state lives in the evaluation context ([`ProbState`]) and is shared by every
//! child context, so a `prob/sample` loop and a lambda body advance one stream. Without `prob/seed` the stream
//! starts from the dispatch's one `now` stamp (`BindingContext::now_millis`): a dispatch replays identically, and
//! the pure evaluator takes no ask to the host.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::*;

fn number(expr: &SExpression, ctx: &BindingContext) -> Result<f64, OrbitalError> {
    Ok(require_number!(eval_expr(expr, ctx)?))
}

fn numbers(value: Value) -> Result<Vec<f64>, OrbitalError> {
    require_array!(value)
        .into_iter()
        .map(|item| Ok(require_number!(item)))
        .collect()
}

fn standard_normal(ctx: &BindingContext) -> Result<f64, OrbitalError> {
    let u1 = ctx.prob_next()?;
    let u2 = ctx.prob_next()?;
    Ok((-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos())
}

/// Marsaglia-Tsang gamma variate; for a shape below one, `gamma(shape + 1) * U^(1/shape)`.
fn gamma_variate(shape: f64, ctx: &BindingContext) -> Result<f64, OrbitalError> {
    if shape.is_nan() {
        return Ok(f64::NAN);
    }
    if shape < 1.0 {
        let u = ctx.prob_next()?;
        return Ok(gamma_variate(shape + 1.0, ctx)? * u.powf(1.0 / shape));
    }
    let d = shape - 1.0 / 3.0;
    let c = 1.0 / (9.0 * d).sqrt();
    loop {
        let (x, mut v) = loop {
            let x = standard_normal(ctx)?;
            let v = 1.0 + c * x;
            if v > 0.0 || v.is_nan() {
                break (x, v);
            }
        };
        v = v * v * v;
        let u = ctx.prob_next()?;
        if u < 1.0 - 0.0331 * (x * x) * (x * x) {
            return Ok(d * v);
        }
        if u.ln() < 0.5 * x * x + d * (1.0 - v + v.ln()) {
            return Ok(d * v);
        }
    }
}

macro_rules! prob_op {
    ($name:ident, $op:literal, $arity:expr, |$args:ident, $ctx:ident| $body:block) => {
        pub struct $name;
        impl Operator for $name {
            fn evaluate(&self, $args: &[SExpression], $ctx: &BindingContext) -> EvalResult {
                require_arity!($args, $op, == $arity);
                $body
            }
        }
    };
}

prob_op!(ProbSeedOp, "prob/seed", 1, |args, ctx| {
    let n = number(&args[0], ctx)?;
    ctx.prob_seed(n);
    Ok(Value::Null)
});

prob_op!(ProbFlipOp, "prob/flip", 1, |args, ctx| {
    let p = number(&args[0], ctx)?;
    Ok(Value::Bool(ctx.prob_next()? < p))
});

prob_op!(ProbGaussianOp, "prob/gaussian", 2, |args, ctx| {
    let mu = number(&args[0], ctx)?;
    let sigma = number(&args[1], ctx)?;
    Ok(Value::Number(mu + sigma * standard_normal(ctx)?))
});

prob_op!(ProbUniformOp, "prob/uniform", 2, |args, ctx| {
    let lo = number(&args[0], ctx)?;
    let hi = number(&args[1], ctx)?;
    Ok(Value::Number(lo + ctx.prob_next()? * (hi - lo)))
});

prob_op!(ProbBetaOp, "prob/beta", 2, |args, ctx| {
    let alpha = number(&args[0], ctx)?;
    let beta = number(&args[1], ctx)?;
    let x = gamma_variate(alpha, ctx)?;
    let y = gamma_variate(beta, ctx)?;
    Ok(Value::Number(x / (x + y)))
});

prob_op!(ProbCategoricalOp, "prob/categorical", 2, |args, ctx| {
    let items = require_array!(eval_expr(&args[0], ctx)?);
    let weights = numbers(eval_expr(&args[1], ctx)?)?;
    if items.len() != weights.len() {
        return Err(EvalError::InvalidArgument(format!(
            "prob/categorical: {} items but {} weights",
            items.len(),
            weights.len()
        ))
        .into());
    }
    let total: f64 = weights.iter().sum();
    if items.is_empty() || weights.iter().any(|w| *w < 0.0 || w.is_nan()) || total <= 0.0 {
        return Err(EvalError::InvalidArgument(
            "prob/categorical: needs at least one item and non-negative weights with a positive sum"
                .to_string(),
        )
        .into());
    }
    let mut r = ctx.prob_next()? * total;
    for (item, weight) in items.iter().zip(&weights) {
        r -= weight;
        if r <= 0.0 {
            return Ok(item.clone());
        }
    }
    Ok(items[items.len() - 1].clone())
});

prob_op!(ProbPoissonOp, "prob/poisson", 1, |args, ctx| {
    let limit = (-number(&args[0], ctx)?).exp();
    let mut k = 0.0;
    let mut p = 1.0;
    loop {
        k += 1.0;
        p *= ctx.prob_next()?;
        if p <= limit || p.is_nan() {
            break;
        }
    }
    Ok(Value::Number(k - 1.0))
});

prob_op!(ProbConditionOp, "prob/condition", 1, |args, ctx| {
    if !eval_expr(&args[0], ctx)?.boolean()? {
        ctx.prob().reject();
    }
    Ok(Value::Null)
});

prob_op!(ProbSampleOp, "prob/sample", 2, |args, ctx| {
    let n = super::array::whole_count(&eval_expr(&args[0], ctx)?, "prob/sample", "n")?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(eval_expr(&args[1], ctx)?);
    }
    Ok(Value::array(out))
});

/// One sample of a model: `set @entity.field` writes the sample's own copy of the entity, under the flat key JS wrote.
/// `do`, `if` and `when` pass their statements through; any other expression is evaluated.
fn run_model(expr: &SExpression, ctx: &mut BindingContext) -> EvalResult {
    let SExpression::Array(items) = expr else {
        return eval_expr(expr, ctx);
    };
    let Some(SExpression::String(head)) = items.first() else {
        return eval_expr(expr, ctx);
    };
    let rest = &items[1..];
    match head.as_str() {
        "do" => {
            let mut last = Value::Null;
            for statement in rest {
                last = run_model(statement, ctx)?;
            }
            Ok(last)
        }
        "set" if rest.len() == 2 => {
            let value = eval_expr(&rest[1], ctx)?;
            if let SExpression::String(target) = &rest[0] {
                if let Some(field) = target.strip_prefix("@entity.") {
                    let mut entity = match ctx.entity.as_value() {
                        Value::Object(map) => map.clone(),
                        _ => ValueMap::default().into(),
                    };
                    entity.insert(field.to_string().into(), value);
                    ctx.set_entity(Value::Object(entity));
                }
            }
            Ok(Value::Null)
        }
        "if" if rest.len() >= 2 => {
            if eval_expr(&rest[0], ctx)?.boolean()? {
                run_model(&rest[1], ctx)
            } else if let Some(otherwise) = rest.get(2) {
                run_model(otherwise, ctx)
            } else {
                Ok(Value::Null)
            }
        }
        "when" if !rest.is_empty() => {
            if eval_expr(&rest[0], ctx)?.boolean()? {
                let mut last = Value::Null;
                for statement in &rest[1..] {
                    last = run_model(statement, ctx)?;
                }
                Ok(last)
            } else {
                Ok(Value::Null)
            }
        }
        _ => eval_expr(expr, ctx),
    }
}

/// Rejection sampling: `n` runs of `model` over a private copy of the entity, keeping the `query` of each run that
/// no `prob/condition` rejected and `evidence` accepts.
fn rejection_samples(
    args: &[SExpression],
    ctx: &BindingContext,
) -> Result<(Vec<Value>, f64), OrbitalError> {
    let n = super::array::whole_count(&eval_expr(&args[3], ctx)?, "prob/posterior", "n")?;
    let outer = ctx.prob().take_rejected();
    let mut accepted = Vec::new();
    for _ in 0..n {
        let mut sample = ctx.child();
        sample.set_entity(match ctx.entity.as_value() {
            Value::Object(map) => Value::Object(map.clone()),
            _ => Value::object(ValueMap::default()),
        });
        ctx.prob().take_rejected();
        run_model(&args[0], &mut sample)?;
        if ctx.prob().take_rejected() {
            continue;
        }
        if !eval_expr(&args[1], &sample)?.boolean()? {
            continue;
        }
        accepted.push(eval_expr(&args[2], &sample)?);
    }
    ctx.prob().restore_rejected(outer);
    Ok((accepted, n as f64))
}

prob_op!(ProbPosteriorOp, "prob/posterior", 4, |args, ctx| {
    Ok(Value::array(rejection_samples(args, ctx)?.0))
});

prob_op!(ProbInferOp, "prob/infer", 4, |args, ctx| {
    let (accepted, n) = rejection_samples(args, ctx)?;
    let samples = numbers(Value::array(accepted))?;
    let count = samples.len() as f64;
    if samples.is_empty() {
        return Err(EvalError::InvalidArgument(
            "prob/infer: no sample was accepted, so there is no mean or variance".to_string(),
        )
        .into());
    }
    let mean = samples.iter().sum::<f64>() / count;
    let variance = samples.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / count;
    let rate = count / n;
    let mut out = ValueMap::default();
    out.insert("mean".into(), Value::Number(mean));
    out.insert("variance".into(), Value::Number(variance));
    out.insert(
        "samples".into(),
        Value::Array(samples.into_iter().map(Value::Number).collect()),
    );
    out.insert("acceptRate".into(), Value::Number(rate));
    Ok(Value::object(out))
});

fn mean_of(operator: &str, samples: &[f64]) -> Result<f64, OrbitalError> {
    if samples.is_empty() {
        return Err(
            EvalError::InvalidArgument(format!("{operator}: needs at least one sample")).into(),
        );
    }
    Ok(samples.iter().sum::<f64>() / samples.len() as f64)
}

prob_op!(
    ProbExpectedValueOp,
    "prob/expected-value",
    1,
    |args, ctx| {
        let samples = numbers(eval_expr(&args[0], ctx)?)?;
        Ok(Value::Number(mean_of("prob/expected-value", &samples)?))
    }
);

prob_op!(ProbVarianceOp, "prob/variance", 1, |args, ctx| {
    let samples = numbers(eval_expr(&args[0], ctx)?)?;
    let mean = mean_of("prob/variance", &samples)?;
    Ok(Value::Number(
        samples.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / samples.len() as f64,
    ))
});

prob_op!(ProbHistogramOp, "prob/histogram", 2, |args, ctx| {
    let samples = numbers(eval_expr(&args[0], ctx)?)?;
    let bins = number(&args[1], ctx)?;
    if bins < 1.0 || bins.fract() != 0.0 {
        return Ok(Value::Null);
    }
    let mut out = ValueMap::default();
    if samples.is_empty() {
        out.insert("binEdges".into(), Value::array(vec![]));
        out.insert("counts".into(), Value::array(vec![]));
        return Ok(Value::object(out));
    }
    let min = samples.iter().copied().fold(f64::INFINITY, f64::min);
    let max = samples.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let width = match (max - min) / bins {
        w if w == 0.0 || w.is_nan() => 1.0,
        w => w,
    };
    let bin_count = bins as usize;
    let mut counts = vec![0.0; bin_count];
    for s in &samples {
        let idx = (((s - min) / width).floor() as usize).min(bin_count - 1);
        counts[idx] += 1.0;
    }
    out.insert(
        "binEdges".into(),
        Value::Array(
            (0..=bin_count)
                .map(|i| Value::Number(min + i as f64 * width))
                .collect(),
        ),
    );
    out.insert(
        "counts".into(),
        Value::Array(counts.into_iter().map(Value::Number).collect()),
    );
    Ok(Value::object(out))
});

fn sorted(mut samples: Vec<f64>) -> Vec<f64> {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    samples
}

prob_op!(ProbPercentileOp, "prob/percentile", 2, |args, ctx| {
    let samples = numbers(eval_expr(&args[0], ctx)?)?;
    let p = number(&args[1], ctx)?;
    if !(0.0..=100.0).contains(&p) {
        return Ok(Value::Null);
    }
    if samples.is_empty() {
        return Ok(Value::Null);
    }
    let samples = sorted(samples);
    let idx = (p / 100.0) * (samples.len() - 1) as f64;
    let (lo, hi) = (idx.floor() as usize, idx.ceil() as usize);
    if lo == hi {
        return Ok(Value::Number(samples[lo]));
    }
    let frac = idx - lo as f64;
    Ok(Value::Number(
        samples[lo] * (1.0 - frac) + samples[hi] * frac,
    ))
});

prob_op!(
    ProbCredibleIntervalOp,
    "prob/credible-interval",
    2,
    |args, ctx| {
        let samples = numbers(eval_expr(&args[0], ctx)?)?;
        let alpha = number(&args[1], ctx)?;
        if !(0.0..=1.0).contains(&alpha) {
            return Ok(Value::Null);
        }
        if samples.is_empty() {
            return Ok(Value::Null);
        }
        let samples = sorted(samples);
        let last = (samples.len() - 1) as f64;
        let lo = ((alpha / 2.0) * last).floor() as usize;
        let hi = ((1.0 - alpha / 2.0) * last).ceil() as usize;
        Ok(Value::array(vec![
            Value::Number(samples[lo]),
            Value::Number(samples[hi]),
        ]))
    }
);

#[cfg(test)]
mod tests {
    use crate::evaluator::context::BindingContextBuilder;
    use crate::evaluator::Evaluator;
    use orbital_core_base::value::Value;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Vector {
        expr: serde_json::Value,
        entity: Option<serde_json::Value>,
        expect: Option<serde_json::Value>,
        #[serde(default)]
        error: bool,
        source: String,
    }

    fn vectors() -> Vec<Vector> {
        serde_json::from_str(include_str!(
            "../../../../../tests/fixtures/operators/prob-vectors.json"
        ))
        .unwrap()
    }

    fn run(
        expr: &serde_json::Value,
        entity: &Option<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let mut builder = BindingContextBuilder::new();
        if let Some(entity) = entity {
            builder = builder.entity(serde_json::from_value::<Value>(entity.clone()).unwrap());
        }
        let sexpr = serde_json::from_value(expr.clone()).unwrap();
        Evaluator::new()
            .evaluate(&sexpr, &builder.build())
            .map(|value| serde_json::to_value(value).unwrap())
            .map_err(|e| e.to_string())
    }

    fn same(a: &serde_json::Value, b: &serde_json::Value) -> bool {
        match (a, b) {
            (serde_json::Value::Number(x), serde_json::Value::Number(y)) => {
                let (x, y) = (x.as_f64().unwrap(), y.as_f64().unwrap());
                (x - y).abs() <= 1e-12 * 1f64.max(x.abs()).max(y.abs())
            }
            (serde_json::Value::Array(x), serde_json::Value::Array(y)) => {
                x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
            }
            (serde_json::Value::Object(x), serde_json::Value::Object(y)) => {
                x.len() == y.len() && x.iter().all(|(k, p)| y.get(k).is_some_and(|q| same(p, q)))
            }
            _ => a == b,
        }
    }

    #[test]
    fn every_shared_prob_vector_matches_the_js_evaluator() {
        let vectors = vectors();
        assert!(vectors.len() > 50, "the fixture is the contract");
        assert!(vectors.iter().any(|v| v.error));
        let mut failures = Vec::new();
        for vector in &vectors {
            let got = run(&vector.expr, &vector.entity);
            if vector.error {
                if let Ok(value) = got {
                    failures.push(format!("{}: want an error, got {value}", vector.source));
                }
                continue;
            }
            let want = vector.expect.clone().unwrap_or(serde_json::Value::Null);
            match got {
                Ok(value) if same(&want, &value) => {}
                Ok(value) => failures.push(format!("{}: want {want}, got {value}", vector.source)),
                Err(e) => failures.push(format!("{}: want {want}, got error {e}", vector.source)),
            }
        }
        assert!(
            failures.is_empty(),
            "{} of {} vectors differ:\n{}",
            failures.len(),
            vectors.len(),
            failures.join("\n")
        );
    }

    #[test]
    fn a_pure_evaluation_without_a_seed_fails_naming_prob_seed() {
        for call in [
            serde_json::json!(["prob/flip", 0.5]),
            serde_json::json!(["prob/sample", 2, ["prob/uniform", 0, 1]]),
            serde_json::json!(["prob/beta", 2, 5]),
        ] {
            let message = run(&call, &None).unwrap_err();
            assert!(message.contains("prob/seed"), "{call}: {message}");
        }
    }

    #[test]
    fn a_dispatch_entropy_source_seeds_an_unseeded_stream_once_and_a_prob_seed_overrides_it() {
        use crate::evaluator::context::EntropySource;
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;
        let asked = Arc::new(AtomicU32::new(0));
        let draw = |seed: u32| {
            let asked = Arc::clone(&asked);
            let mut ctx = BindingContextBuilder::new().build();
            ctx.set_prob_entropy(EntropySource::new(move || {
                asked.fetch_add(1, Ordering::SeqCst);
                Ok(seed)
            }));
            let expr = serde_json::from_value(serde_json::json!([
                "prob/sample",
                3,
                ["prob/uniform", 0, 1]
            ]))
            .unwrap();
            serde_json::to_value(Evaluator::new().evaluate(&expr, &ctx).unwrap()).unwrap()
        };
        let a = draw(7);
        assert_eq!(asked.load(Ordering::SeqCst), 1, "one ask for three draws");
        assert_eq!(a, draw(7), "the same answer replays the same values");
        assert_ne!(a, draw(8), "a different answer draws different values");
        let mut ctx = BindingContextBuilder::new().build();
        ctx.set_prob_entropy(EntropySource::new(|| Err("must not be asked".to_string())));
        let seeded = serde_json::from_value(serde_json::json!([
            "do",
            ["prob/seed", 42],
            ["prob/uniform", 0, 1]
        ]))
        .unwrap();
        assert!(
            Evaluator::new().evaluate(&seeded, &ctx).is_ok(),
            "a prob/seed never asks the source"
        );
    }

    #[test]
    fn control_a_non_prob_call_is_untouched_by_the_prob_scope() {
        let got = run(&serde_json::json!(["+", 1, 2]), &None).unwrap();
        assert!(same(&got, &serde_json::json!(3)));
    }

    #[test]
    fn empty_statistics_and_mismatched_weights_are_refused() {
        let ok = |e: serde_json::Value| run(&e, &None).unwrap();
        let err = |e: serde_json::Value| run(&e, &None).is_err();
        assert!(same(
            &ok(serde_json::json!(["prob/expected-value", [1, 3]])),
            &serde_json::json!(2)
        ));
        assert!(same(
            &ok(serde_json::json!(["prob/variance", [1, 3]])),
            &serde_json::json!(1)
        ));
        assert!(err(serde_json::json!(["prob/expected-value", []])));
        assert!(err(serde_json::json!(["prob/variance", []])));
        assert!(ok(serde_json::json!(["prob/percentile", [], 50])).is_null());
        assert!(ok(serde_json::json!(["prob/credible-interval", [], 0.05])).is_null());
        let seeded = |items: serde_json::Value, weights: serde_json::Value| {
            serde_json::json!(["do", ["prob/seed", 1], ["prob/categorical", items, weights]])
        };
        assert!(!err(seeded(
            serde_json::json!(["a", "b"]),
            serde_json::json!([1, 2])
        )));
        assert!(err(seeded(
            serde_json::json!(["a", "b"]),
            serde_json::json!([1])
        )));
        assert!(err(seeded(
            serde_json::json!(["a", "b"]),
            serde_json::json!([0, 0])
        )));
        assert!(err(seeded(
            serde_json::json!(["a", "b"]),
            serde_json::json!([-1, 2])
        )));
        assert!(err(serde_json::json!([
            "do",
            ["prob/seed", 1],
            ["prob/sample", -3, ["prob/flip", 0.5]]
        ])));
        assert!(!err(serde_json::json!([
            "do",
            ["prob/seed", 1],
            ["prob/sample", 0, ["prob/flip", 0.5]]
        ])));
    }
}
