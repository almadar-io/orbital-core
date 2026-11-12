//! `signal/*` pure analysis. PCM is a mem block of f32 little-endian samples, interleaved by channel.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::*;

fn invalid(op: &str, detail: impl std::fmt::Display) -> OrbitalError {
    OrbitalError::Eval(EvalError::InvalidArgument(format!("{op}: {detail}")))
}

struct Framing {
    channels: usize,
    channel: Option<usize>,
    window: usize,
    hop: usize,
    start: usize,
    count: Option<usize>,
}

fn field_int(spec: &Value, key: &str, op: &str) -> Result<i64, OrbitalError> {
    spec.get(key)
        .ok_or_else(|| invalid(op, format!("the spec needs {key}")))?
        .int()
        .map_err(OrbitalError::Eval)
}

fn optional_index(spec: &Value, key: &str, op: &str) -> Result<Option<usize>, OrbitalError> {
    match spec.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let n = value.int().map_err(OrbitalError::Eval)?;
            usize::try_from(n)
                .map(Some)
                .map_err(|_| invalid(op, format!("{key} must not be negative")))
        }
    }
}

fn framing(spec: &Value, op: &str, power_of_two: bool) -> Result<Framing, OrbitalError> {
    let channels = field_int(spec, "channels", op)?;
    if channels < 1 {
        return Err(invalid(op, "channels must be at least 1"));
    }
    let channel = field_int(spec, "channel", op)?;
    if channel < -1 || channel >= channels {
        return Err(invalid(op, "channel must be -1 or an index below channels"));
    }
    let window = field_int(spec, "window", op)?;
    if power_of_two {
        if window < 2 || (window & (window - 1)) != 0 {
            return Err(invalid(op, "window must be a power of two of at least 2"));
        }
    } else if window < 1 {
        return Err(invalid(op, "window must be at least 1"));
    }
    let hop = field_int(spec, "hop", op)?;
    if hop < 1 {
        return Err(invalid(op, "hop must be at least 1"));
    }
    let to_usize = |n: i64, what: &str| {
        usize::try_from(n).map_err(|_| invalid(op, format!("{what} is out of range")))
    };
    Ok(Framing {
        channels: to_usize(channels, "channels")?,
        channel: usize::try_from(channel).ok(),
        window: to_usize(window, "window")?,
        hop: to_usize(hop, "hop")?,
        start: optional_index(spec, "start", op)?.unwrap_or(0),
        count: optional_index(spec, "count", op)?,
    })
}

struct Pcm {
    data: Vec<u8>,
    channels: usize,
    channel: Option<usize>,
}

impl Pcm {
    fn mono_len(&self) -> usize {
        self.data.len() / (4 * self.channels)
    }

    fn sample(&self, frame: usize, channel: usize) -> f64 {
        let at = (frame * self.channels + channel) * 4;
        match self
            .data
            .get(at..at + 4)
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        {
            Some(bytes) => f64::from(f32::from_le_bytes(bytes)),
            None => 0.0,
        }
    }

    fn mono(&self, index: usize) -> f64 {
        if index >= self.mono_len() {
            return 0.0;
        }
        match self.channel {
            Some(channel) => self.sample(index, channel),
            None => {
                (0..self.channels)
                    .map(|channel| self.sample(index, channel))
                    .sum::<f64>()
                    / self.channels as f64
            }
        }
    }
}

fn load(
    args: &[SExpression],
    ctx: &BindingContext,
    op: &str,
    power_of_two: bool,
) -> Result<(Pcm, Framing, Vec<usize>), OrbitalError> {
    let borrow = match eval_expr(&args[0], ctx)? {
        handle @ Value::Sealed(_) => handle,
        other => return Err(other.kind_error("Borrow").into()),
    };
    let spec = eval_expr(&args[1], ctx)?;
    let framing = framing(&spec, op, power_of_two)?;
    let memory = ctx.kernel_mem(op)?;
    let length = memory.length(&borrow).map_err(OrbitalError::Eval)?;
    let data = memory
        .read(&borrow, 0, length)
        .map_err(OrbitalError::Eval)?;
    if data.len() % (4 * framing.channels) != 0 {
        return Err(invalid(op, "the block is not a whole number of f32 frames"));
    }
    let pcm = Pcm {
        data,
        channels: framing.channels,
        channel: framing.channel,
    };
    let total = pcm.mono_len().div_ceil(framing.hop);
    let first = framing.start.min(total);
    let last = framing
        .count
        .map_or(total, |count| first.saturating_add(count).min(total));
    Ok((pcm, framing, (first..last).collect()))
}

fn sample_rate_of(spec: &Value, op: &str) -> Result<f64, OrbitalError> {
    let rate = field_int(spec, "sampleRate", op)?;
    if rate < 1 {
        return Err(invalid(op, "sampleRate must be at least 1"));
    }
    Ok(rate as f64)
}

fn bands_of(spec: &Value, op: &str) -> Result<Vec<(f64, f64)>, OrbitalError> {
    let list = spec
        .get("bands")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid(op, "the spec needs bands, a list of [lowHz, highHz]"))?;
    list.iter()
        .map(|pair| {
            let edges = pair
                .as_array()
                .filter(|edges| edges.len() == 2)
                .ok_or_else(|| invalid(op, "a band is a [lowHz, highHz] pair"))?;
            let low = edges[0].number().map_err(OrbitalError::Eval)?;
            let high = edges[1].number().map_err(OrbitalError::Eval)?;
            if low < 0.0 || high < 0.0 || low >= high {
                return Err(invalid(op, "a band needs 0 <= lowHz < highHz"));
            }
            Ok((low, high))
        })
        .collect()
}

fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -2.0 * core::f64::consts::PI / len as f64;
        let (step_re, step_im) = (angle.cos(), angle.sin());
        for chunk in (0..n).step_by(len) {
            let (mut w_re, mut w_im) = (1.0, 0.0);
            for k in 0..len / 2 {
                let (a, b) = (chunk + k, chunk + k + len / 2);
                let t_re = re[b] * w_re - im[b] * w_im;
                let t_im = re[b] * w_im + im[b] * w_re;
                re[b] = re[a] - t_re;
                im[b] = im[a] - t_im;
                re[a] += t_re;
                im[a] += t_im;
                let next = w_re * step_re - w_im * step_im;
                w_im = w_re * step_im + w_im * step_re;
                w_re = next;
            }
        }
        len <<= 1;
    }
}

/// signal/frames (borrow, spec) - one row per analysis frame of the PCM block, one mean FFT magnitude per band
pub struct SignalFramesOp;
impl Operator for SignalFramesOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "signal/frames";
        require_arity!(args, OP, == 2);
        let spec = eval_expr(&args[1], ctx)?;
        let rate = sample_rate_of(&spec, OP)?;
        let bands = bands_of(&spec, OP)?;
        let (pcm, framing, frames) = load(args, ctx, OP, true)?;
        let window = framing.window;
        let hann: Vec<f64> = (0..window)
            .map(|n| 0.5 - 0.5 * (2.0 * core::f64::consts::PI * n as f64 / window as f64).cos())
            .collect();
        let members: Vec<Vec<usize>> = bands
            .iter()
            .map(|(low, high)| {
                (0..=window / 2)
                    .filter(|k| (*low..*high).contains(&(*k as f64 * rate / window as f64)))
                    .collect()
            })
            .collect();
        let mut re = vec![0.0; window];
        let mut im = vec![0.0; window];
        let mut rows = Vec::with_capacity(frames.len());
        for frame in frames {
            let origin = frame * framing.hop;
            for n in 0..window {
                re[n] = pcm.mono(origin + n) * hann[n];
                im[n] = 0.0;
            }
            fft(&mut re, &mut im);
            let row = members
                .iter()
                .map(|bins| {
                    if bins.is_empty() {
                        return Value::Number(0.0);
                    }
                    let sum: f64 = bins
                        .iter()
                        .map(|k| re[*k].hypot(im[*k]) / window as f64)
                        .sum();
                    Value::Number(sum / bins.len() as f64)
                })
                .collect::<Vec<Value>>();
            rows.push(Value::array(row));
        }
        Ok(Value::array(rows))
    }
}

/// signal/envelope (borrow, spec) - the RMS of each analysis frame of the PCM block
pub struct SignalEnvelopeOp;
impl Operator for SignalEnvelopeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "signal/envelope";
        require_arity!(args, OP, == 2);
        let (pcm, framing, frames) = load(args, ctx, OP, false)?;
        let values = frames
            .into_iter()
            .map(|frame| {
                let origin = frame * framing.hop;
                let energy: f64 = (0..framing.window)
                    .map(|n| pcm.mono(origin + n).powi(2))
                    .sum();
                Value::Number((energy / framing.window as f64).sqrt())
            })
            .collect::<Vec<Value>>();
        Ok(Value::array(values))
    }
}

/// signal/autocorrelate (xs, minLag, maxLag) - the mean lagged product of a list of numbers, one value per lag
pub struct SignalAutocorrelateOp;
impl Operator for SignalAutocorrelateOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "signal/autocorrelate";
        require_arity!(args, OP, == 3);
        let list = eval_expr(&args[0], ctx)?;
        let xs = list
            .as_array()
            .ok_or_else(|| OrbitalError::from(list.kind_error("array")))?
            .iter()
            .map(|x| x.number().map_err(OrbitalError::Eval))
            .collect::<Result<Vec<f64>, OrbitalError>>()?;
        let min_lag = eval_expr(&args[1], ctx)?.int()?;
        let max_lag = eval_expr(&args[2], ctx)?.int()?;
        if min_lag < 0 || max_lag < min_lag {
            return Err(invalid(OP, "needs 0 <= minLag <= maxLag"));
        }
        let n = xs.len();
        let values = (min_lag..=max_lag)
            .map(|lag| {
                let lag = usize::try_from(lag).unwrap_or(usize::MAX);
                if lag >= n {
                    return Value::Number(0.0);
                }
                let sum: f64 = (0..n - lag).map(|i| xs[i] * xs[i + lag]).sum();
                Value::Number(sum / (n - lag) as f64)
            })
            .collect::<Vec<Value>>();
        Ok(Value::array(values))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::evaluator::context::KernelMem;
    use orbital_core_base::value::{value_map_with_capacity, ValueMap};

    #[derive(Debug)]
    struct Fake(Vec<u8>);

    impl KernelMem for Fake {
        fn read(&self, borrow: &Value, offset: i64, length: i64) -> Result<Vec<u8>, EvalError> {
            if !borrow.is_sealed_as("Borrow") {
                return Err(borrow.kind_error("Borrow"));
            }
            let start =
                usize::try_from(offset).map_err(|_| EvalError::InvalidArgument("offset".into()))?;
            let len =
                usize::try_from(length).map_err(|_| EvalError::InvalidArgument("length".into()))?;
            self.0
                .get(start..start + len)
                .map(<[u8]>::to_vec)
                .ok_or_else(|| EvalError::InvalidArgument("out of bounds".into()))
        }

        fn length(&self, handle: &Value) -> Result<i64, EvalError> {
            if handle.is_sealed_as("Borrow") || handle.is_sealed_as("Block") {
                Ok(self.0.len() as i64)
            } else {
                Err(handle.kind_error("Block or Borrow"))
            }
        }
    }

    fn sealed(kind: &'static str) -> Value {
        let mut fields: ValueMap = value_map_with_capacity(1);
        fields.insert("id".to_string().into(), Value::Int(1));
        Value::sealed(kind, fields)
    }

    fn pcm_bytes(samples: &[f32]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    fn record(fields: Vec<(&str, Value)>) -> Value {
        let mut map: ValueMap = value_map_with_capacity(fields.len());
        for (key, value) in fields {
            map.insert(key.to_string().into(), value);
        }
        Value::object(map)
    }

    fn band(low: f64, high: f64) -> Value {
        Value::array(vec![Value::Number(low), Value::Number(high)])
    }

    fn spec(window: i64, hop: i64, channel: i64, bands: Vec<Value>) -> Vec<(&'static str, Value)> {
        vec![
            ("sampleRate", Value::Int(8000)),
            ("channels", Value::Int(1)),
            ("channel", Value::Int(channel)),
            ("window", Value::Int(window)),
            ("hop", Value::Int(hop)),
            ("bands", Value::array(bands)),
        ]
    }

    fn ctx_with(data: Vec<u8>, spec: Vec<(&str, Value)>) -> BindingContext {
        let mut ctx = BindingContext::default();
        ctx.set_kernel_mem(Arc::new(Fake(data)));
        ctx.set_local("b", sealed("Borrow"));
        ctx.set_local("spec", record(spec));
        ctx
    }

    fn run(op: &str, ctx: &BindingContext) -> EvalResult {
        let expr: SExpression =
            serde_json::from_value(serde_json::json!([op, "@b", "@spec"])).expect("s-expression");
        eval_expr(&expr, ctx)
    }

    fn rows(value: Value) -> Vec<Vec<f64>> {
        value
            .as_array()
            .expect("list")
            .iter()
            .map(|row| {
                row.as_array()
                    .expect("row")
                    .iter()
                    .map(|n| n.as_number().expect("number"))
                    .collect()
            })
            .collect()
    }

    fn numbers(value: Value) -> Vec<f64> {
        value
            .as_array()
            .expect("list")
            .iter()
            .map(|n| n.as_number().expect("number"))
            .collect()
    }

    fn sine(hz: f64, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| (2.0 * std::f64::consts::PI * hz * i as f64 / 8000.0).sin() as f32)
            .collect()
    }

    #[test]
    fn a_sine_lands_in_its_band_and_silence_is_zero() {
        let bands = vec![band(0.0, 500.0), band(900.0, 1100.0), band(2000.0, 3000.0)];
        let ctx = ctx_with(
            pcm_bytes(&sine(1000.0, 1024)),
            spec(256, 256, 0, bands.clone()),
        );
        let out = rows(run("signal/frames", &ctx).unwrap());
        assert_eq!(out.len(), 4);
        for row in &out {
            assert!(row[1] > 10.0 * row[0] && row[1] > 10.0 * row[2], "{row:?}");
        }
        let silent = ctx_with(pcm_bytes(&vec![0.0; 1024]), spec(256, 256, 0, bands));
        assert!(
            rows(run("signal/frames", &silent).unwrap())
                .iter()
                .flatten()
                .all(|v| *v == 0.0),
            "control: silence"
        );
    }

    #[test]
    fn frame_count_is_ceil_and_the_tail_is_zero_padded() {
        let ctx = ctx_with(
            pcm_bytes(&[1.0; 300]),
            spec(128, 100, 0, vec![band(0.0, 1.0)]),
        );
        let out = rows(run("signal/frames", &ctx).unwrap());
        assert_eq!(out.len(), 3, "ceil(300 / 100)");
        assert!(
            (out[0][0] - 0.5).abs() < 1e-6,
            "a full frame of ones: the Hann DC magnitude is 0.5: {out:?}"
        );
        assert!(
            out[2][0] < out[0][0],
            "the last frame holds 100 real samples then padding: {out:?}"
        );
        let empty = ctx_with(vec![], spec(128, 100, 0, vec![band(0.0, 4000.0)]));
        assert!(
            rows(run("signal/frames", &empty).unwrap()).is_empty(),
            "control: no samples, no frames"
        );
    }

    #[test]
    fn start_and_count_select_frames() {
        let mut fields = spec(128, 100, 0, vec![band(0.0, 4000.0)]);
        fields.push(("start", Value::Int(1)));
        fields.push(("count", Value::Int(1)));
        let ctx = ctx_with(pcm_bytes(&sine(1000.0, 300)), fields);
        assert_eq!(rows(run("signal/frames", &ctx).unwrap()).len(), 1);
    }

    #[test]
    fn a_band_without_bins_is_zero_and_channel_mean_reads_every_channel() {
        let mut fields = spec(8, 8, -1, vec![band(10.0, 20.0)]);
        fields.retain(|(k, _)| *k != "channels");
        fields.push(("channels", Value::Int(2)));
        let ctx = ctx_with(pcm_bytes(&[0.5; 16]), fields);
        assert_eq!(
            rows(run("signal/frames", &ctx).unwrap()),
            vec![vec![0.0]],
            "bins sit 1000 Hz apart, none in 10..20"
        );
        let mut with_dc = spec(8, 8, -1, vec![band(0.0, 500.0)]);
        with_dc.retain(|(k, _)| *k != "channels");
        with_dc.push(("channels", Value::Int(2)));
        let dc = ctx_with(pcm_bytes(&[0.5; 16]), with_dc);
        assert!(
            rows(run("signal/frames", &dc).unwrap())[0][0] > 0.1,
            "control: the DC bin is in 0..500"
        );
    }

    #[test]
    fn frames_refuses_bad_specs() {
        let good = || spec(256, 128, 0, vec![band(0.0, 100.0)]);
        let data = pcm_bytes(&sine(100.0, 512));
        let with = |key: &'static str, value: Value| {
            let mut fields = good();
            fields.retain(|(k, _)| *k != key);
            fields.push((key, value));
            fields
        };
        assert!(
            run("signal/frames", &ctx_with(data.clone(), good())).is_ok(),
            "control"
        );
        let refusals = vec![
            with("window", Value::Int(100)),
            with("hop", Value::Int(0)),
            with("channels", Value::Int(0)),
            with("channel", Value::Int(1)),
            with("channel", Value::Int(-2)),
            with("sampleRate", Value::Int(0)),
            with("bands", Value::array(vec![band(100.0, 100.0)])),
            with("bands", Value::array(vec![band(-1.0, 100.0)])),
        ];
        for fields in refusals {
            assert!(matches!(
                run("signal/frames", &ctx_with(data.clone(), fields)),
                Err(OrbitalError::Eval(EvalError::InvalidArgument(_)))
            ));
        }
        let mut stereo = good();
        stereo.retain(|(k, _)| *k != "channels");
        stereo.push(("channels", Value::Int(2)));
        assert!(
            run("signal/frames", &ctx_with(vec![0; 12], stereo)).is_err(),
            "12 bytes is not a multiple of 8"
        );
    }

    #[test]
    fn envelope_of_a_constant_is_the_constant_and_window_need_not_be_a_power_of_two() {
        let mut fields = spec(100, 100, 0, vec![]);
        fields.retain(|(k, _)| *k != "bands");
        let ctx = ctx_with(pcm_bytes(&vec![0.5; 300]), fields.clone());
        let out = numbers(run("signal/envelope", &ctx).unwrap());
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|v| (v - 0.5).abs() < 1e-6), "{out:?}");
        let quiet = ctx_with(pcm_bytes(&vec![0.0; 300]), fields);
        assert!(
            numbers(run("signal/envelope", &quiet).unwrap())
                .iter()
                .all(|v| *v == 0.0),
            "control"
        );
    }

    #[test]
    fn envelope_refuses_a_zero_hop_and_a_misaligned_block() {
        let mut fields = spec(100, 0, 0, vec![]);
        fields.retain(|(k, _)| *k != "bands");
        assert!(run("signal/envelope", &ctx_with(pcm_bytes(&[0.5; 4]), fields)).is_err());
        let mut ok = spec(2, 2, 0, vec![]);
        ok.retain(|(k, _)| *k != "bands");
        assert!(run("signal/envelope", &ctx_with(vec![0; 5], ok)).is_err());
    }

    fn autocorrelate(xs: Vec<f64>, min: i64, max: i64) -> EvalResult {
        let mut ctx = BindingContext::default();
        ctx.set_local(
            "xs",
            Value::array(xs.into_iter().map(Value::Number).collect::<Vec<_>>()),
        );
        let expr: SExpression =
            serde_json::from_value(serde_json::json!(["signal/autocorrelate", "@xs", min, max]))
                .expect("s-expression");
        eval_expr(&expr, &ctx)
    }

    #[test]
    fn autocorrelation_of_a_period_four_pulse_train_peaks_at_lag_four() {
        let xs: Vec<f64> = (0..32)
            .map(|i| if i % 4 == 0 { 1.0 } else { 0.0 })
            .collect();
        let r = numbers(autocorrelate(xs, 1, 8).unwrap());
        assert_eq!(r.len(), 8);
        assert!((r[3] - 0.25).abs() < 1e-9, "lag 4: {r:?}");
        assert!(r[2] < 1e-9, "control: lag 3 is lower: {r:?}");
        assert!((r[7] - 0.25).abs() < 1e-9, "lag 8");
    }

    #[test]
    fn a_lag_past_the_end_is_zero_and_bad_lags_refuse() {
        assert_eq!(
            numbers(autocorrelate(vec![1.0, 2.0], 0, 3).unwrap()),
            vec![2.5, 2.0, 0.0, 0.0]
        );
        assert!(autocorrelate(vec![1.0], -1, 2).is_err());
        assert!(autocorrelate(vec![1.0], 3, 2).is_err());
    }
}
