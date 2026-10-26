//! System information: the monotonic clock, users, CPU, memory, disks and network.

use super::*;

pub(super) fn clock_monotonic() -> Value {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    let origin = *ORIGIN.get_or_init(Instant::now);
    object([("nanos", num(origin.elapsed().as_nanos() as f64))])
}

pub(super) fn uid() -> Result<Value, OsError> {
    Ok(object([("uid", num(posix::uid()))]))
}

pub(super) fn gid() -> Result<Value, OsError> {
    Ok(object([("gid", num(posix::gid()))]))
}

pub(super) fn username(op: OsOp) -> Result<Value, OsError> {
    Ok(object([("username", text(posix::username(op)?))]))
}

pub(super) fn group_of(op: OsOp, args: &[Value]) -> Result<Value, OsError> {
    let (gid, members) = posix::group_of(op, arg_str(args, 0, op)?)?;
    Ok(object([
        ("gid", num(gid)),
        (
            "members",
            Value::Array(members.into_iter().map(text).collect()),
        ),
    ]))
}

pub(super) fn cpu_usage(op: OsOp) -> Result<Value, OsError> {
    Ok(object([("percent", num(native::cpu_percent(op)?))]))
}

pub(super) fn memory_info(op: OsOp) -> Result<Value, OsError> {
    let (total, available) = native::memory(op)?;
    let used = (total - available).max(0.0);
    Ok(object([
        ("total", num(total)),
        ("available", num(available)),
        ("used", num(used)),
        ("usedPercent", num(percent_of(used, total))),
    ]))
}

pub(super) fn uptime(op: OsOp) -> Result<Value, OsError> {
    Ok(object([("seconds", num(native::uptime_seconds(op)?))]))
}

pub(super) fn load_avg(op: OsOp) -> Result<Value, OsError> {
    let [one, five, fifteen] = posix::load_avg(op)?;
    Ok(object([
        ("one", num(one)),
        ("five", num(five)),
        ("fifteen", num(fifteen)),
    ]))
}

pub(super) fn hostname(op: OsOp) -> Result<Value, OsError> {
    Ok(object([("hostname", text(posix::hostname(op)?))]))
}

pub(super) fn uname(op: OsOp) -> Result<Value, OsError> {
    let [sysname, nodename, release, version, machine] = posix::uname(op)?;
    Ok(object([
        ("sysname", text(sysname)),
        ("nodename", text(nodename)),
        ("release", text(release)),
        ("version", text(version)),
        ("machine", text(machine)),
    ]))
}

pub(super) fn disk_usage(op: OsOp, args: &[Value]) -> Result<Value, OsError> {
    let path = arg_str(args, 0, op)?;
    let (total, used, available) = posix::disk_space(op, path)?;
    Ok(object([
        ("path", text(path)),
        ("total", num(total)),
        ("used", num(used)),
        ("available", num(available)),
        ("usedPercent", num(percent_of(used, total))),
        ("filesystem", text(native::filesystem_of(op, path)?)),
    ]))
}

pub(super) fn mounts(op: OsOp) -> Result<Value, OsError> {
    Ok(object([("mounts", native::mounts(op)?)]))
}

pub(super) fn disk_io(op: OsOp) -> Result<Value, OsError> {
    let devices = procfs::disk_io(&read_small(op, "/proc/diskstats")?);
    Ok(object([("devices", Value::array(devices))]))
}

pub(super) fn network_interfaces(op: OsOp) -> Result<Value, OsError> {
    let list = native::interfaces(op)?
        .into_iter()
        .map(|i| {
            object([
                ("name", text(i.name)),
                ("mac", text(i.mac)),
                ("up", Value::Bool(i.up)),
                ("running", Value::Bool(i.running)),
            ])
        })
        .collect();
    Ok(object([("interfaces", Value::Array(list))]))
}

pub(super) fn network_stats(op: OsOp) -> Result<Value, OsError> {
    let stats = procfs::net_dev(&read_small(op, "/proc/net/dev")?);
    Ok(object([("interfaces", Value::object(stats))]))
}

pub(super) fn network_connected(op: OsOp) -> Result<Value, OsError> {
    Ok(object([("connected", Value::Bool(native::connected(op)?))]))
}

pub(super) fn default_gateway(op: OsOp) -> Result<Value, OsError> {
    let gateway = procfs::default_gateway(&read_small(op, "/proc/net/route")?);
    Ok(object([("gateway", gateway.map_or(Value::Null, text))]))
}
