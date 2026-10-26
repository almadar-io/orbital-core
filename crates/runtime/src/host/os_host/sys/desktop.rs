//! Desktop actions through the platform's tools: notify, speak, brightness, volume and sleep.

use super::*;

pub(super) fn run_tool(op: OsOp, program: &str, args: &[&str]) -> Result<String, OsError> {
    match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
    {
        Ok(out) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        Ok(out) => Err(err(
            op,
            OsErrorKind::Io,
            format!(
                "{program} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(unsupported(op, format!("{program} is not installed")))
        }
        Err(e) => Err(io_error(op, &e)),
    }
}

/// The first tool of `chain` that is installed runs; none installed is `unsupported`.
pub(super) fn run_chain(op: OsOp, chain: &[(&str, Vec<String>)]) -> Result<String, OsError> {
    for (program, args) in chain {
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        match run_tool(op, program, &borrowed) {
            Err(e) if e.kind == OsErrorKind::Unsupported => continue,
            other => return other,
        }
    }
    let names: Vec<&str> = chain.iter().map(|(p, _)| *p).collect();
    Err(unsupported(
        op,
        format!("no supported tool is installed ({})", names.join(", ")),
    ))
}

pub(super) fn tool(program: &'static str, args: &[&str]) -> (&'static str, Vec<String>) {
    (program, args.iter().map(|a| (*a).to_string()).collect())
}

pub(super) fn osascript(script: &str) -> (&'static str, Vec<String>) {
    tool("osascript", &["-e", script])
}

pub(super) fn level_arg(args: &[Value], i: usize, op: OsOp) -> Result<f64, OsError> {
    match args.get(i) {
        Some(Value::Number(n)) if n.is_finite() && (0.0..=100.0).contains(n) => Ok(*n),
        _ => Err(err(
            op,
            OsErrorKind::Invalid,
            format!("argument {} must be a number from 0 to 100", i + 1),
        )),
    }
}

pub(super) fn first_percent(raw: &str) -> Option<f64> {
    let at = raw.find('%')?;
    let digits: String = raw[..at]
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    digits.parse().ok()
}

pub(super) fn notify(op: OsOp, args: &[Value]) -> Result<Value, OsError> {
    let title = arg_str(args, 0, op)?;
    let body = arg_str(args, 1, op)?;
    let urgency = match arg_opt(args, 2) {
        None => "normal",
        Some(_) => match arg_str(args, 2, op)? {
            u @ ("low" | "normal" | "critical") => u,
            _ => {
                return Err(err(
                    op,
                    OsErrorKind::Invalid,
                    "the urgency must be low, normal or critical",
                ))
            }
        },
    };
    run_chain(
        op,
        &[
            tool("notify-send", &["--urgency", urgency, "--", title, body]),
            tool(
                "osascript",
                &[
                    "-e",
                    "on run argv",
                    "-e",
                    "display notification (item 2 of argv) with title (item 1 of argv)",
                    "-e",
                    "end run",
                    "--",
                    title,
                    body,
                ],
            ),
        ],
    )?;
    Ok(object([]))
}

pub(super) fn speak(op: OsOp, args: &[Value]) -> Result<Value, OsError> {
    let words = arg_str(args, 0, op)?;
    run_chain(
        op,
        &[
            tool("espeak-ng", &["--", words]),
            tool("espeak", &["--", words]),
            tool("say", &["--", words]),
        ],
    )?;
    Ok(object([]))
}

pub(super) fn brightness_get(op: OsOp) -> Result<Value, OsError> {
    const BACKLIGHT: &str = "/sys/class/backlight";
    for name in dir_names(op, BACKLIGHT)? {
        let base = format!("{BACKLIGHT}/{name}");
        if let (Some(current), Some(max)) = (
            read_f64(&format!("{base}/brightness")),
            read_f64(&format!("{base}/max_brightness")),
        ) {
            if max > 0.0 {
                return Ok(object([("level", num((current / max * 100.0).round()))]));
            }
        }
    }
    Err(unsupported(op, "no backlight device reports a brightness"))
}

pub(super) fn brightness_set(op: OsOp, args: &[Value]) -> Result<Value, OsError> {
    let level = level_arg(args, 0, op)?;
    run_chain(
        op,
        &[tool(
            "brightnessctl",
            &["set", &format!("{}%", level as u64)],
        )],
    )?;
    Ok(object([]))
}

pub(super) fn volume_get(op: OsOp) -> Result<Value, OsError> {
    let raw = run_chain(
        op,
        &[
            tool("pactl", &["get-sink-volume", "@DEFAULT_SINK@"]),
            tool("amixer", &["get", "Master"]),
            osascript("output volume of (get volume settings)"),
        ],
    )?;
    let level = first_percent(&raw)
        .or_else(|| raw.trim().parse::<f64>().ok())
        .ok_or_else(|| unsupported(op, "the host reports no output volume"))?;
    Ok(object([("level", num(level))]))
}

pub(super) fn volume_set(op: OsOp, args: &[Value]) -> Result<Value, OsError> {
    let percent = level_arg(args, 0, op)? as u64;
    run_chain(
        op,
        &[
            tool(
                "pactl",
                &["set-sink-volume", "@DEFAULT_SINK@", &format!("{percent}%")],
            ),
            tool("amixer", &["set", "Master", &format!("{percent}%")]),
            osascript(&format!("set volume output volume {percent}")),
        ],
    )?;
    Ok(object([]))
}

pub(super) fn volume_mute(op: OsOp, args: &[Value]) -> Result<Value, OsError> {
    let muted = arg_bool(args, 0, op)?;
    run_chain(
        op,
        &[
            tool(
                "pactl",
                &[
                    "set-sink-mute",
                    "@DEFAULT_SINK@",
                    if muted { "1" } else { "0" },
                ],
            ),
            tool(
                "amixer",
                &["set", "Master", if muted { "mute" } else { "unmute" }],
            ),
            osascript(if muted {
                "set volume output muted true"
            } else {
                "set volume output muted false"
            }),
        ],
    )?;
    Ok(object([]))
}

pub(super) fn suspend(op: OsOp) -> Result<Value, OsError> {
    run_chain(
        op,
        &[
            tool("systemctl", &["suspend"]),
            tool("pm-suspend", &[]),
            tool("pmset", &["sleepnow"]),
        ],
    )?;
    Ok(object([]))
}

pub(super) fn hibernate(op: OsOp) -> Result<Value, OsError> {
    run_chain(
        op,
        &[tool("systemctl", &["hibernate"]), tool("pm-hibernate", &[])],
    )?;
    Ok(object([]))
}
