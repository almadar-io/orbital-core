//! The `gpu/` family (EXEC-BOUND-2): the kernel checks every handle against its own table, keeps each device inside the
//! bytes its grant allows, validates the command list, and reads the bytes of a `Borrow` out of its own memory. The
//! host's answerer only ever sees plain ids and bytes; the sealed handles it is answered with are minted here from the
//! ids it returns. The table is staged with the step like `mem/*`, so a step that parks and replays records once.

use super::mem_table::{as_int64, closed, non_negative, refuse, sealed};
use super::*;
use orbital_core_base::os::{family_of, OsFamily, OsOp};
use orbital_core_base::os_grants::{gpu_target, GpuKind, OsRequirement};
use orbital_core_base::sealed::{
    GPU_BIND_GROUP, GPU_BUFFER, GPU_DEVICE, GPU_PIPELINE, GPU_QUERY_SET, GPU_SAMPLER, GPU_SHADER,
    GPU_TEXTURE,
};
use orbital_core_base::sync::Mutex;

const HANDLE_KINDS: [&str; 7] = [
    GPU_BUFFER,
    GPU_TEXTURE,
    GPU_SAMPLER,
    GPU_SHADER,
    GPU_PIPELINE,
    GPU_BIND_GROUP,
    GPU_QUERY_SET,
];

fn invalid(message: impl Into<String>) -> OsError {
    refuse(OsErrorKind::Invalid, message)
}

#[derive(Debug, Clone)]
struct DeviceEntry {
    max_bytes: u64,
    used: u64,
    live: bool,
}

#[derive(Debug, Clone)]
struct HandleEntry {
    kind: &'static str,
    device: i64,
    bytes: u64,
    live: bool,
}

/// Every device and handle the host answered, with the bytes each device holds. A released or closed entry stays, not
/// live, so a later use answers `closed`.
#[derive(Debug, Clone, Default)]
pub(super) struct GpuTable {
    devices: BTreeMap<i64, DeviceEntry>,
    handles: BTreeMap<i64, HandleEntry>,
}

fn id_of(value: Option<&Value>, kind: &str) -> Result<i64, OsError> {
    value
        .filter(|value| value.is_sealed_as(kind))
        .and_then(|value| value.get("id"))
        .and_then(Value::as_int)
        .ok_or_else(|| invalid(format!("expects a {kind} handle")))
}

impl GpuTable {
    fn device(&self, id: i64) -> Result<&DeviceEntry, OsError> {
        self.devices
            .get(&id)
            .filter(|device| device.live)
            .ok_or_else(|| closed(GPU_DEVICE, id))
    }

    fn device_handle(&self, value: Option<&Value>) -> Result<(i64, &DeviceEntry), OsError> {
        let id = id_of(value, GPU_DEVICE)?;
        Ok((id, self.device(id)?))
    }

    fn handle(
        &self,
        value: Option<&Value>,
        kind: &'static str,
    ) -> Result<(i64, &HandleEntry), OsError> {
        let id = id_of(value, kind)?;
        let entry = self
            .handles
            .get(&id)
            .filter(|entry| entry.kind == kind && entry.live)
            .ok_or_else(|| closed(kind, id))?;
        self.device(entry.device).map_err(|_| closed(kind, id))?;
        Ok((id, entry))
    }

    fn handle_of_any(
        &self,
        value: Option<&Value>,
        kinds: &[&'static str],
    ) -> Result<(i64, &HandleEntry), OsError> {
        let kind = kinds
            .iter()
            .copied()
            .find(|kind| value.is_some_and(|value| value.is_sealed_as(kind)))
            .ok_or_else(|| invalid(format!("expects a {} handle", kinds.join(" or "))))?;
        self.handle(value, kind)
    }

    fn add_device(&mut self, id: i64, max_bytes: u64) -> Result<(), OsError> {
        if self.devices.contains_key(&id) {
            return Err(refuse(
                OsErrorKind::Io,
                format!("the host answered the device id {id} twice"),
            ));
        }
        self.devices.insert(
            id,
            DeviceEntry {
                max_bytes,
                used: 0,
                live: true,
            },
        );
        Ok(())
    }

    fn add_handle(
        &mut self,
        id: i64,
        kind: &'static str,
        device: i64,
        bytes: u64,
    ) -> Result<(), OsError> {
        if self.handles.contains_key(&id) {
            return Err(refuse(
                OsErrorKind::Io,
                format!("the host answered the handle id {id} twice"),
            ));
        }
        let entry = self
            .devices
            .get_mut(&device)
            .filter(|entry| entry.live)
            .ok_or_else(|| closed(GPU_DEVICE, device))?;
        entry.used = entry.used.saturating_add(bytes);
        self.handles.insert(
            id,
            HandleEntry {
                kind,
                device,
                bytes,
                live: true,
            },
        );
        Ok(())
    }

    fn release(&mut self, id: i64) {
        let Some(entry) = self.handles.get_mut(&id).filter(|entry| entry.live) else {
            return;
        };
        entry.live = false;
        let (device, bytes) = (entry.device, entry.bytes);
        if let Some(device) = self.devices.get_mut(&device) {
            device.used = device.used.saturating_sub(bytes);
        }
    }

    fn close_device(&mut self, id: i64) {
        if let Some(device) = self.devices.get_mut(&id) {
            device.live = false;
            device.used = 0;
        }
        for entry in self.handles.values_mut().filter(|entry| entry.device == id) {
            entry.live = false;
        }
    }

    /// Whether `bytes` more fit the device's granted `maxBytes`.
    fn check_budget(&self, device: i64, bytes: u64) -> Result<(), OsError> {
        let entry = self.device(device)?;
        match entry.used.checked_add(bytes) {
            Some(total) if total <= entry.max_bytes => Ok(()),
            _ => Err(invalid(format!(
                "{bytes} more bytes go beyond the granted maxBytes {}",
                entry.max_bytes
            ))),
        }
    }
}

#[derive(Debug, Default)]
struct GpuInner {
    committed: GpuTable,
    staged: Option<GpuTable>,
    /// A keyed `gpu/device-open` the host deferred: the `maxBytes` its grant gave, until the completion mints the device.
    pending: BTreeMap<String, u64>,
}

/// The kernel's gpu state: the committed table, the running step's staged copy and the deferred opens, behind one lock
/// so a completion registering a device cannot be lost to a step committing.
#[derive(Debug, Default)]
pub(super) struct GpuState {
    inner: Mutex<GpuInner>,
}

impl GpuState {
    fn seen<R>(&self, f: impl FnOnce(&GpuTable) -> R) -> R {
        let inner = self.inner.lock();
        f(inner.staged.as_ref().unwrap_or(&inner.committed))
    }

    fn staged<R>(&self, f: impl FnOnce(&mut GpuTable) -> R) -> R {
        let mut inner = self.inner.lock();
        let inner = &mut *inner;
        let committed = &inner.committed;
        f(inner.staged.get_or_insert_with(|| committed.clone()))
    }

    pub(super) fn settle(&self, committed: bool) {
        let mut inner = self.inner.lock();
        if let (Some(table), true) = (inner.staged.take(), committed) {
            inner.committed = table;
        }
    }

    fn defer_open(&self, key: &str, max_bytes: u64) {
        self.inner.lock().pending.insert(key.to_string(), max_bytes);
    }

    fn take_open(&self, key: &str) -> Option<u64> {
        self.inner.lock().pending.remove(key)
    }

    /// Register a device a completion opened, in the committed table and in the running step's staged one.
    fn register_device(&self, id: i64, max_bytes: u64) -> Result<(), OsError> {
        let mut inner = self.inner.lock();
        inner.committed.add_device(id, max_bytes)?;
        if let Some(staged) = inner.staged.as_mut() {
            staged.add_device(id, max_bytes)?;
        }
        Ok(())
    }
}

/// What to do with the host's answer, decided before the ask.
enum After {
    Nothing,
    Open {
        max_bytes: u64,
    },
    Close {
        device: i64,
    },
    Release {
        id: i64,
    },
    Mint {
        field: &'static str,
        device: i64,
        bytes: u64,
        minted: Minted,
    },
}

enum Minted {
    Buffer {
        size: i64,
    },
    Texture {
        width: i64,
        height: i64,
        format: Value,
    },
    Sampler,
    Shader,
    Pipeline {
        kind: Value,
    },
    BindGroup,
    QuerySet {
        count: i64,
    },
}

impl Minted {
    fn kind(&self) -> &'static str {
        match self {
            Minted::Buffer { .. } => GPU_BUFFER,
            Minted::Texture { .. } => GPU_TEXTURE,
            Minted::Sampler => GPU_SAMPLER,
            Minted::Shader => GPU_SHADER,
            Minted::Pipeline { .. } => GPU_PIPELINE,
            Minted::BindGroup => GPU_BIND_GROUP,
            Minted::QuerySet { .. } => GPU_QUERY_SET,
        }
    }

    fn value(&self, id: i64, device: i64) -> Value {
        let (id, device) = (Value::Int(id), Value::Int(device));
        match self {
            Minted::Buffer { size } => sealed(
                GPU_BUFFER,
                [("id", id), ("device", device), ("size", Value::Int(*size))],
            ),
            Minted::Texture {
                width,
                height,
                format,
            } => sealed(
                GPU_TEXTURE,
                [
                    ("id", id),
                    ("device", device),
                    ("width", Value::Int(*width)),
                    ("height", Value::Int(*height)),
                    ("format", format.clone()),
                ],
            ),
            Minted::Sampler => sealed(GPU_SAMPLER, [("id", id), ("device", device)]),
            Minted::Shader => sealed(GPU_SHADER, [("id", id), ("device", device)]),
            Minted::Pipeline { kind } => sealed(
                GPU_PIPELINE,
                [("id", id), ("device", device), ("kind", kind.clone())],
            ),
            Minted::BindGroup => sealed(GPU_BIND_GROUP, [("id", id), ("device", device)]),
            Minted::QuerySet { count } => sealed(
                GPU_QUERY_SET,
                [
                    ("id", id),
                    ("device", device),
                    ("count", Value::Int(*count)),
                ],
            ),
        }
    }
}

struct Plan {
    ask: Vec<Value>,
    after: After,
}

/// `answer` with `field` replaced by `value`.
fn with_field(answer: &Value, field: &str, value: Value) -> Result<Value, OsError> {
    let mut fields = answer.as_object().cloned().ok_or_else(|| {
        refuse(
            OsErrorKind::Io,
            "the host answered a gpu call with something that is not a record",
        )
    })?;
    fields.insert(field.into(), value);
    Ok(Value::object(fields))
}

fn answered_id(answer: &Value, field: &str) -> Result<i64, OsError> {
    answer.get(field).and_then(Value::as_int).ok_or_else(|| {
        refuse(
            OsErrorKind::Io,
            format!("the host answered no `{field}` id"),
        )
    })
}

/// The one place a GpuDevice is minted: the sync answer of `gpu/device-open` and its deferred completion both come here.
fn mint_device(answer: &Value, max_bytes: u64) -> Result<(i64, Value), OsError> {
    let id = answered_id(answer, "device")?;
    let device = sealed(
        GPU_DEVICE,
        [
            ("id", Value::Int(id)),
            ("maxBytes", Value::Int(as_int64(max_bytes)?)),
        ],
    );
    Ok((id, with_field(answer, "device", device)?))
}

fn record<'a>(
    value: Option<&'a Value>,
    what: &str,
    required: &[&str],
    optional: &[&str],
) -> Result<&'a ValueMap, OsError> {
    let map = value
        .filter(|value| matches!(value, Value::Object(_)))
        .and_then(Value::as_object)
        .ok_or_else(|| invalid(format!("{what} must be a record")))?;
    if let Some((key, _)) = map
        .iter()
        .find(|(key, _)| !required.contains(&key.as_str()) && !optional.contains(&key.as_str()))
    {
        return Err(invalid(format!("{what} has no field `{key}`")));
    }
    if let Some(missing) = required.iter().find(|key| map.get(**key).is_none()) {
        return Err(invalid(format!("{what} needs `{missing}`")));
    }
    Ok(map)
}

fn text<'a>(map: &'a ValueMap, key: &str) -> Result<&'a str, OsError> {
    map.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("`{key}` must be text")))
}

fn optional_text<'a>(
    map: &'a ValueMap,
    key: &str,
    allowed: &[&str],
) -> Result<Option<&'a str>, OsError> {
    match map.get(key) {
        None => Ok(None),
        Some(value) => match value.as_str() {
            Some(text) if allowed.is_empty() || allowed.contains(&text) => Ok(Some(text)),
            _ => Err(invalid(if allowed.is_empty() {
                format!("`{key}` must be text")
            } else {
                format!("`{key}` must be one of {}", allowed.join(", "))
            })),
        },
    }
}

fn text_list(map: &ValueMap, key: &str) -> Result<(), OsError> {
    match map.get(key) {
        None => Ok(()),
        Some(value) => match value.as_array() {
            Some(items) if items.iter().all(|item| item.as_str().is_some()) => Ok(()),
            _ => Err(invalid(format!("`{key}` must be a list of text"))),
        },
    }
}

/// `length` bytes at `offset` fit in `total`.
fn within(offset: u64, length: u64, total: u64) -> Result<(), OsError> {
    match offset.checked_add(length) {
        Some(end) if end <= total => Ok(()),
        _ => Err(invalid(format!("the range goes beyond the buffer's {total} bytes"))),
    }
}

fn natural(map: &ValueMap, key: &str) -> Result<u64, OsError> {
    non_negative(map.get(key), key)
}

fn optional_natural(map: &ValueMap, key: &str) -> Result<Option<u64>, OsError> {
    map.get(key)
        .map(|value| non_negative(Some(value), key))
        .transpose()
}

fn texel_bytes(format: &str) -> Option<u64> {
    match format {
        "r8unorm" => Some(1),
        "rg8unorm" | "r16float" => Some(2),
        "rgba8unorm" | "rgba8unorm-srgb" | "bgra8unorm" | "bgra8unorm-srgb" | "rgba8uint"
        | "rgba8sint" | "rg16float" | "r32float" | "r32uint" | "r32sint" | "depth32float" => {
            Some(4)
        }
        "rgba16float" | "rg32float" => Some(8),
        "rgba32float" | "rgba32uint" | "rgba32sint" => Some(16),
        _ => None,
    }
}

/// The exact bytes of a texture: the sum over its mip levels of the level's texels times the format's texel size.
fn texture_bytes(width: u64, height: u64, depth: u64, mips: u64, texel: u64) -> Option<u64> {
    let extent = |size: u64, level: u64| {
        u32::try_from(level)
            .ok()
            .and_then(|level| size.checked_shr(level))
            .unwrap_or(0)
            .max(1)
    };
    let mut total: u64 = 0;
    for level in 0..mips {
        let bytes = extent(width, level)
            .checked_mul(extent(height, level))?
            .checked_mul(extent(depth, level))?
            .checked_mul(texel)?;
        total = total.checked_add(bytes)?;
    }
    Some(total)
}

/// The handle arguments of one device, each checked live and on that device.
struct Scope<'a> {
    kernel: &'a RuntimeKernel,
    table: &'a GpuTable,
    device: i64,
}

impl Scope<'_> {
    fn id(&self, value: Option<&Value>, kinds: &[&'static str]) -> Result<Value, OsError> {
        let (id, entry) = self.table.handle_of_any(value, kinds)?;
        if entry.device != self.device {
            return Err(invalid(format!(
                "the {} handle {id} belongs to another device",
                entry.kind
            )));
        }
        Ok(Value::Int(id))
    }

    fn owned(&self, id: i64, entry: &HandleEntry) -> Result<(), OsError> {
        if entry.device != self.device {
            return Err(invalid(format!(
                "the {} handle {id} belongs to another device",
                entry.kind
            )));
        }
        Ok(())
    }

    /// A write's bytes: bytes pass through as they are, a Borrow lends its block's bytes, base64 text is decoded.
    fn data(&self, value: Option<&Value>) -> Result<Value, OsError> {
        Ok(match value {
            Some(bytes @ Value::Bytes(_)) => bytes.clone(),
            Some(borrow) if borrow.is_sealed_as("Borrow") => {
                Value::bytes(self.kernel.borrow_bytes(borrow, false)?)
            }
            Some(data) => Value::bytes(
                data.bytes_view()
                    .map_err(|e| invalid(format!("the data must be bytes or a Borrow: {e}")))?
                    .into_owned(),
            ),
            None => return Err(invalid("a write command needs `data`")),
        })
    }

    fn ids(&self, value: Option<&Value>, what: &str, kind: &'static str) -> Result<Value, OsError> {
        let items = value
            .and_then(Value::as_array)
            .ok_or_else(|| invalid(format!("`{what}` must be a list")))?;
        items
            .iter()
            .map(|item| self.id(Some(item), &[kind]))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::array)
    }

    fn command(&self, value: &Value) -> Result<(Value, Option<GpuKind>), OsError> {
        let map = value
            .as_object()
            .filter(|_| matches!(value, Value::Object(_)))
            .ok_or_else(|| invalid("a gpu command must be a record"))?;
        let mut entries = map.iter();
        let (Some((name, body)), None) = (entries.next(), entries.next()) else {
            return Err(invalid(
                "a gpu command holds exactly one of write, compute, render, copy, clear, timestamp or read",
            ));
        };
        let buffer_or_texture = [GPU_BUFFER, GPU_TEXTURE];
        let (need, inner) =
            match name.as_str() {
                "write" => {
                    let body = record(
                        Some(body),
                        "a write command",
                        &["data"],
                        &["buffer", "texture", "offset"],
                    )?;
                    let data = self.data(body.get("data"))?;
                    let length = data.as_bytes().map_or(0, <[u8]>::len) as u64;
                    let mut fields = ValueMap::default();
                    match (body.get("buffer"), body.get("texture")) {
                        (Some(buffer), None) => {
                            let (id, entry) = self.table.handle(Some(buffer), GPU_BUFFER)?;
                            self.owned(id, entry)?;
                            let offset = optional_natural(body, "offset")?.unwrap_or(0);
                            within(offset, length, entry.bytes)?;
                            fields.insert("buffer".into(), Value::Int(id));
                            fields.insert("offset".into(), Value::Int(as_int64(offset)?));
                        }
                        (None, Some(texture)) => {
                            if body.get("offset").is_some() {
                                return Err(invalid("a texture write carries the whole texture and takes no `offset`"));
                            }
                            fields.insert("texture".into(), self.id(Some(texture), &[GPU_TEXTURE])?);
                        }
                        _ => return Err(invalid("a write command names exactly one of `buffer` or `texture`")),
                    }
                    fields.insert("data".into(), data);
                    (None, fields)
                }
                "read" => {
                    let body = record(Some(body), "a read command", &["buffer"], &["offset", "length"])?;
                    let (id, entry) = self.table.handle(body.get("buffer"), GPU_BUFFER)?;
                    self.owned(id, entry)?;
                    let offset = optional_natural(body, "offset")?.unwrap_or(0);
                    let length = match optional_natural(body, "length")? {
                        Some(length) => length,
                        None => entry.bytes.checked_sub(offset).ok_or_else(|| {
                            invalid(format!("the offset is past the buffer's {} bytes", entry.bytes))
                        })?,
                    };
                    within(offset, length, entry.bytes)?;
                    let fields: ValueMap = [
                        ("buffer".into(), Value::Int(id)),
                        ("offset".into(), Value::Int(as_int64(offset)?)),
                        ("length".into(), Value::Int(as_int64(length)?)),
                    ]
                    .into_iter()
                    .collect();
                    (Some(GpuKind::Readback), fields)
                }
                "compute" => {
                    let body = record(
                        Some(body),
                        "a compute command",
                        &["pipeline", "bindGroups", "dispatch"],
                        &[],
                    )?;
                    let dispatch = body
                        .get("dispatch")
                        .and_then(Value::as_array)
                        .filter(|groups| groups.len() == 3);
                    let dispatch =
                        dispatch.ok_or_else(|| invalid("`dispatch` must be three counts"))?;
                    for count in dispatch {
                        non_negative(Some(count), "a dispatch count")?;
                    }
                    let fields: ValueMap = [
                        (
                            "pipeline".into(),
                            self.id(body.get("pipeline"), &[GPU_PIPELINE])?,
                        ),
                        (
                            "bindGroups".into(),
                            self.ids(body.get("bindGroups"), "bindGroups", GPU_BIND_GROUP)?,
                        ),
                        ("dispatch".into(), Value::array(dispatch.clone())),
                    ]
                    .into_iter()
                    .collect();
                    (Some(GpuKind::Compute), fields)
                }
                "render" => {
                    let body = record(
                        Some(body),
                        "a render command",
                        &["pipeline", "bindGroups", "target", "vertices"],
                        &["instances", "clear"],
                    )?;
                    natural(body, "vertices")?;
                    let mut fields: ValueMap = [
                        (
                            "pipeline".into(),
                            self.id(body.get("pipeline"), &[GPU_PIPELINE])?,
                        ),
                        (
                            "bindGroups".into(),
                            self.ids(body.get("bindGroups"), "bindGroups", GPU_BIND_GROUP)?,
                        ),
                        (
                            "target".into(),
                            self.id(body.get("target"), &[GPU_TEXTURE])?,
                        ),
                        (
                            "vertices".into(),
                            Value::Int(as_int64(natural(body, "vertices")?)?),
                        ),
                    ]
                    .into_iter()
                    .collect();
                    if let Some(instances) = optional_natural(body, "instances")? {
                        fields.insert("instances".into(), Value::Int(as_int64(instances)?));
                    }
                    if let Some(clear) = body.get("clear") {
                        let channels = clear.as_array().filter(|channels| {
                            channels.len() == 4
                                && channels.iter().all(|channel| channel.as_number().is_some())
                        });
                        channels.ok_or_else(|| invalid("`clear` must be four numbers"))?;
                        fields.insert("clear".into(), clear.clone());
                    }
                    (Some(GpuKind::Render), fields)
                }
                "copy" => {
                    let body = record(
                        Some(body),
                        "a copy command",
                        &["from", "to"],
                        &["fromOffset", "toOffset", "size"],
                    )?;
                    let mut fields: ValueMap = [
                        (
                            "from".into(),
                            self.id(body.get("from"), &buffer_or_texture)?,
                        ),
                        ("to".into(), self.id(body.get("to"), &buffer_or_texture)?),
                    ]
                    .into_iter()
                    .collect();
                    for key in ["fromOffset", "toOffset", "size"] {
                        if let Some(count) = optional_natural(body, key)? {
                            fields.insert(key.into(), Value::Int(as_int64(count)?));
                        }
                    }
                    (None, fields)
                }
                "clear" => {
                    let body = record(
                        Some(body),
                        "a clear command",
                        &["buffer"],
                        &["offset", "size"],
                    )?;
                    let mut fields: ValueMap =
                        [("buffer".into(), self.id(body.get("buffer"), &[GPU_BUFFER])?)]
                            .into_iter()
                            .collect();
                    for key in ["offset", "size"] {
                        if let Some(count) = optional_natural(body, key)? {
                            fields.insert(key.into(), Value::Int(as_int64(count)?));
                        }
                    }
                    (None, fields)
                }
                "timestamp" => {
                    let body = record(
                        Some(body),
                        "a timestamp command",
                        &["querySet", "index"],
                        &[],
                    )?;
                    let fields: ValueMap = [
                        (
                            "querySet".into(),
                            self.id(body.get("querySet"), &[GPU_QUERY_SET])?,
                        ),
                        (
                            "index".into(),
                            Value::Int(as_int64(natural(body, "index")?)?),
                        ),
                    ]
                    .into_iter()
                    .collect();
                    (None, fields)
                }
                _ => return Err(invalid(
                    "a gpu command holds exactly one of write, compute, render, copy, clear, timestamp or read",
                )),
            };
        let command: ValueMap = [(name.as_str().into(), Value::object(inner))]
            .into_iter()
            .collect();
        Ok((Value::object(command), need))
    }
}

impl RuntimeKernel {
    fn gpu_granted(&self, orbital: Option<&str>, kind: GpuKind) -> Result<(), OsError> {
        if self
            .grants
            .os_grants
            .read()
            .allows(orbital, &OsRequirement::Gpu { kind })
        {
            return Ok(());
        }
        Err(refuse(
            OsErrorKind::Denied,
            format!("no gpu grant lists `{}`", kind.name()),
        ))
    }

    /// The `maxBytes` of the first granted device that lists `open`.
    fn gpu_max_bytes(&self, orbital: Option<&str>) -> Result<u64, OsError> {
        let grants = self.grants.os_grants.read();
        grants
            .for_orbital(orbital)
            .gpu
            .iter()
            .filter(|grant| grant.kinds.contains(&GpuKind::Open))
            .find_map(|grant| gpu_target(&grant.target))
            .map(|target| target.max_bytes)
            .ok_or_else(|| refuse(OsErrorKind::Denied, "no gpu grant lists `open`"))
    }

    /// Answer a `gpu/*` op: validate it against the table and the grants, unwrap its handles to ids, ask the host, and
    /// mint the sealed handles of its answer. `None` for an op outside the family. A keyed `gpu/device-open` the host
    /// defers answers later as a completion; [`Self::gpu_complete`] mints its device.
    pub(super) fn gpu_os(
        &self,
        op: OsOp,
        args: &[Value],
        host_key: Option<&str>,
        principal: Option<&str>,
        orbital: Option<&str>,
    ) -> Option<Result<Value, OsError>> {
        if family_of(op) != OsFamily::Gpu {
            return None;
        }
        let run = || -> Result<Value, OsError> {
            let plan = self
                .gpu
                .seen(|table| self.gpu_plan(table, op, args, orbital))?;
            let answer = self.io.os(op, &plan.ask, host_key, principal, orbital)?;
            let deferred =
                answer.is_null() && self.io.peek_call_verdict() == Some(CallVerdict::Deferred);
            self.gpu_answered(plan.after, answer, deferred, host_key)
        };
        Some(
            run().map_err(|error| {
                OsError::new(error.kind, format!("{}: {}", op.name(), error.message))
            }),
        )
    }

    fn gpu_answered(
        &self,
        after: After,
        answer: Value,
        deferred: bool,
        host_key: Option<&str>,
    ) -> Result<Value, OsError> {
        if deferred {
            if let (After::Open { max_bytes }, Some(key)) = (&after, host_key) {
                self.gpu.defer_open(key, *max_bytes);
            }
            return Ok(answer);
        }
        match after {
            After::Nothing => Ok(answer),
            After::Open { max_bytes } => {
                let (id, minted) = mint_device(&answer, max_bytes)?;
                self.gpu.staged(|table| table.add_device(id, max_bytes))?;
                Ok(minted)
            }
            After::Close { device } => {
                self.gpu.staged(|table| table.close_device(device));
                Ok(answer)
            }
            After::Release { id } => {
                self.gpu.staged(|table| table.release(id));
                Ok(answer)
            }
            After::Mint {
                field,
                device,
                bytes,
                minted,
            } => {
                let id = answered_id(&answer, field)?;
                self.gpu
                    .staged(|table| table.add_handle(id, minted.kind(), device, bytes))?;
                with_field(&answer, field, minted.value(id, device))
            }
        }
    }

    /// The completion of a deferred `gpu/device-open` under host key `key`: the device is minted and registered. Any other
    /// key's outcome is returned as it stands.
    pub fn gpu_complete(
        &self,
        key: &str,
        outcome: Result<Value, OsError>,
    ) -> Result<Value, OsError> {
        let Some(max_bytes) = self.gpu.take_open(key) else {
            return outcome;
        };
        let prefixed = |error: OsError| {
            OsError::new(
                error.kind,
                format!("{}: {}", OsOp::GpuDeviceOpen.name(), error.message),
            )
        };
        let (id, minted) = mint_device(&outcome?, max_bytes).map_err(prefixed)?;
        self.gpu.register_device(id, max_bytes).map_err(prefixed)?;
        Ok(minted)
    }

    /// End the step: its gpu table changes take hold when `committed`, and are dropped otherwise.
    pub(super) fn settle_gpu(&self, committed: bool) {
        self.gpu.settle(committed);
    }

    fn gpu_plan(
        &self,
        table: &GpuTable,
        op: OsOp,
        args: &[Value],
        orbital: Option<&str>,
    ) -> Result<Plan, OsError> {
        let max = match op {
            OsOp::GpuDeviceOpen | OsOp::GpuDeviceClose | OsOp::GpuRelease => 1,
            OsOp::GpuBufferCreate
            | OsOp::GpuTextureCreate
            | OsOp::GpuSamplerCreate
            | OsOp::GpuQueryCreate
            | OsOp::GpuShaderCreate
            | OsOp::GpuPipelineCreate
            | OsOp::GpuSubmit => 2,
            OsOp::GpuBindGroupCreate => 3,
            _ => return Err(refuse(OsErrorKind::Unsupported, "not a gpu op")),
        };
        if args.len() != max {
            return Err(invalid(format!(
                "takes {max} arguments, got {}",
                args.len()
            )));
        }
        let plain = |ask: Vec<Value>| Plan {
            ask,
            after: After::Nothing,
        };
        let created =
            |ask: Vec<Value>, field: &'static str, device: i64, bytes: u64, minted: Minted| Plan {
                ask,
                after: After::Mint {
                    field,
                    device,
                    bytes,
                    minted,
                },
            };
        match op {
            OsOp::GpuDeviceOpen => {
                let desc = record(
                    args.first(),
                    "the device description",
                    &[],
                    &["powerPreference", "features"],
                )?;
                optional_text(desc, "powerPreference", &["low-power", "high-performance"])?;
                text_list(desc, "features")?;
                self.gpu_granted(orbital, GpuKind::Open)?;
                let max_bytes = self.gpu_max_bytes(orbital)?;
                Ok(Plan {
                    ask: vec![args[0].clone()],
                    after: After::Open { max_bytes },
                })
            }
            OsOp::GpuDeviceClose => {
                let (device, _) = table.device_handle(args.first())?;
                Ok(Plan {
                    ask: vec![Value::Int(device)],
                    after: After::Close { device },
                })
            }
            OsOp::GpuRelease => {
                if args[0].is_sealed_as(GPU_DEVICE) {
                    return Err(invalid("a device is closed with gpu/device-close"));
                }
                let (id, _) = table.handle_of_any(args.first(), &HANDLE_KINDS)?;
                Ok(Plan {
                    ask: vec![Value::Int(id)],
                    after: After::Release { id },
                })
            }
            OsOp::GpuBufferCreate => {
                let (device, _) = table.device_handle(args.first())?;
                let desc = record(
                    args.get(1),
                    "the buffer description",
                    &["size", "usage"],
                    &[],
                )?;
                let size = natural(desc, "size")?;
                text_list(desc, "usage")?;
                table.check_budget(device, size)?;
                let minted = Minted::Buffer {
                    size: as_int64(size)?,
                };
                Ok(created(
                    vec![Value::Int(device), args[1].clone()],
                    "buffer",
                    device,
                    size,
                    minted,
                ))
            }
            OsOp::GpuTextureCreate => {
                let (device, _) = table.device_handle(args.first())?;
                let desc = record(
                    args.get(1),
                    "the texture description",
                    &["width", "height", "format", "usage"],
                    &["depth", "mips"],
                )?;
                let positive = |key: &str, default: Option<u64>| -> Result<u64, OsError> {
                    let found = match default {
                        Some(default) => optional_natural(desc, key)?.unwrap_or(default),
                        None => natural(desc, key)?,
                    };
                    if found == 0 {
                        return Err(invalid(format!("`{key}` must be at least 1")));
                    }
                    Ok(found)
                };
                let (width, height) = (positive("width", None)?, positive("height", None)?);
                let (depth, mips) = (positive("depth", Some(1))?, positive("mips", Some(1))?);
                if mips > 32 {
                    return Err(invalid("`mips` is at most 32"));
                }
                let format = text(desc, "format")?;
                let texel = texel_bytes(format)
                    .ok_or_else(|| invalid(format!("`{format}` is not a texture format")))?;
                text_list(desc, "usage")?;
                let bytes = texture_bytes(width, height, depth, mips, texel)
                    .ok_or_else(|| invalid("the texture size exceeds the int range"))?;
                table.check_budget(device, bytes)?;
                let minted = Minted::Texture {
                    width: as_int64(width)?,
                    height: as_int64(height)?,
                    format: Value::string(format.to_string()),
                };
                Ok(created(
                    vec![Value::Int(device), args[1].clone()],
                    "texture",
                    device,
                    bytes,
                    minted,
                ))
            }
            OsOp::GpuSamplerCreate => {
                let (device, _) = table.device_handle(args.first())?;
                let desc = record(
                    args.get(1),
                    "the sampler description",
                    &[],
                    &["filter", "address"],
                )?;
                optional_text(desc, "filter", &["nearest", "linear"])?;
                optional_text(desc, "address", &["clamp", "repeat", "mirror"])?;
                Ok(created(
                    vec![Value::Int(device), args[1].clone()],
                    "sampler",
                    device,
                    0,
                    Minted::Sampler,
                ))
            }
            OsOp::GpuQueryCreate => {
                let (device, _) = table.device_handle(args.first())?;
                let desc = record(args.get(1), "the query description", &["count"], &[])?;
                let count = natural(desc, "count")?;
                if count == 0 {
                    return Err(invalid("`count` must be at least 1"));
                }
                let minted = Minted::QuerySet {
                    count: as_int64(count)?,
                };
                Ok(created(
                    vec![Value::Int(device), args[1].clone()],
                    "querySet",
                    device,
                    0,
                    minted,
                ))
            }
            OsOp::GpuShaderCreate => {
                let (device, _) = table.device_handle(args.first())?;
                if args[1].as_str().is_none() {
                    return Err(invalid("the shader source must be text"));
                }
                Ok(created(
                    vec![Value::Int(device), args[1].clone()],
                    "shader",
                    device,
                    0,
                    Minted::Shader,
                ))
            }
            OsOp::GpuPipelineCreate => {
                let (device, _) = table.device_handle(args.first())?;
                let desc = record(
                    args.get(1),
                    "the pipeline description",
                    &["kind", "shader", "entry"],
                    &["fragment", "targets"],
                )?;
                let kind = optional_text(desc, "kind", &["compute", "render"])?
                    .ok_or_else(|| invalid("the pipeline description needs `kind`"))?;
                text(desc, "entry")?;
                optional_text(desc, "fragment", &[])?;
                text_list(desc, "targets")?;
                let shader = Scope { kernel: self, table, device }.id(desc.get("shader"), &[GPU_SHADER])?;
                let mut ask: ValueMap = desc.clone();
                ask.insert("shader".into(), shader);
                let minted = Minted::Pipeline {
                    kind: Value::string(kind.to_string()),
                };
                Ok(created(
                    vec![Value::Int(device), Value::object(ask)],
                    "pipeline",
                    device,
                    0,
                    minted,
                ))
            }
            OsOp::GpuBindGroupCreate => {
                let (pipeline, entry) = table.handle(args.first(), GPU_PIPELINE)?;
                let device = entry.device;
                let group = natural_value(args.get(1), "the group")?;
                let scope = Scope { kernel: self, table, device };
                let entries = args[2]
                    .as_array()
                    .ok_or_else(|| invalid("the entries must be a list"))?;
                let mut bound = Vec::with_capacity(entries.len());
                for entry in entries {
                    let map = record(
                        Some(entry),
                        "a binding",
                        &["binding"],
                        &["buffer", "texture", "sampler", "offset", "size"],
                    )?;
                    let resources = ["buffer", "texture", "sampler"]
                        .into_iter()
                        .filter(|key| map.get(*key).is_some())
                        .count();
                    if resources != 1 {
                        return Err(invalid(
                            "a binding names exactly one of buffer, texture or sampler",
                        ));
                    }
                    let mut fields: ValueMap = [(
                        "binding".into(),
                        Value::Int(as_int64(natural(map, "binding")?)?),
                    )]
                    .into_iter()
                    .collect();
                    for (key, kind) in [
                        ("buffer", GPU_BUFFER),
                        ("texture", GPU_TEXTURE),
                        ("sampler", GPU_SAMPLER),
                    ] {
                        if map.get(key).is_some() {
                            fields.insert(key.into(), scope.id(map.get(key), &[kind])?);
                        }
                    }
                    for key in ["offset", "size"] {
                        if let Some(count) = optional_natural(map, key)? {
                            fields.insert(key.into(), Value::Int(as_int64(count)?));
                        }
                    }
                    bound.push(Value::object(fields));
                }
                Ok(created(
                    vec![Value::Int(pipeline), group, Value::array(bound)],
                    "bindGroup",
                    device,
                    0,
                    Minted::BindGroup,
                ))
            }
            OsOp::GpuSubmit => {
                let (device, _) = table.device_handle(args.first())?;
                let commands = args[1]
                    .as_array()
                    .ok_or_else(|| invalid("the commands must be a list"))?;
                let scope = Scope { kernel: self, table, device };
                let mut resolved = Vec::with_capacity(commands.len());
                let mut needs: Vec<GpuKind> = Vec::new();
                for command in commands {
                    let (command, need) = scope.command(command)?;
                    if let Some(need) = need.filter(|need| !needs.contains(need)) {
                        needs.push(need);
                    }
                    resolved.push(command);
                }
                for need in needs {
                    self.gpu_granted(orbital, need)?;
                }
                Ok(plain(vec![Value::Int(device), Value::array(resolved)]))
            }
            _ => Err(refuse(OsErrorKind::Unsupported, "not a gpu op")),
        }
    }
}

fn natural_value(value: Option<&Value>, what: &str) -> Result<Value, OsError> {
    Ok(Value::Int(as_int64(non_negative(value, what)?)?))
}
