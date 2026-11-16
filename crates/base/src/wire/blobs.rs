//! Blobs (EXEC-DIST-16b): a browser-bound answer carries each SExpr-typed emit payload value as `{ "$blob": n }`, its
//! bytes after the JSON head in a length-framed body. The browser host keeps the bytes; its kernel holds a handle.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use core::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, Weak};

/// Key of a blob leaf.
pub const BLOB_MARKER: &str = "$blob";

/// Media type of a framed answer body: `u32` LE head length, the JSON head, then per blob a `u32` LE length and its bytes.
pub const FRAMED_MEDIA_TYPE: &str = "application/vnd.almadar.framed";

/// A value whose bytes only the host holds, named by the id the host handed this kernel.
#[derive(Debug, PartialEq, Eq)]
pub struct Blob {
    id: u64,
}

impl Blob {
    pub fn id(&self) -> u64 {
        self.id
    }
}

/// The blobs one host handed a kernel: a weak handle per id, and the ids the kernel let go of since the host asked.
#[derive(Default)]
pub struct Blobs {
    live: BTreeMap<u64, Weak<Blob>>,
    released: Vec<u64>,
}

impl Blobs {
    pub fn new() -> Self {
        Self::default()
    }

    /// The ids whose blobs the kernel no longer holds, oldest first; the host drops their bytes.
    pub fn take_released(&mut self) -> Vec<u64> {
        let dead: Vec<u64> = self
            .live
            .iter()
            .filter(|(_, w)| w.strong_count() == 0)
            .map(|(id, _)| *id)
            .collect();
        for id in dead {
            self.live.remove(&id);
            self.released.push(id);
        }
        core::mem::take(&mut self.released)
    }

    /// How many blobs the kernel still holds.
    pub fn held(&self) -> usize {
        self.live.values().filter(|w| w.strong_count() > 0).count()
    }
}

struct ReadScope {
    handed: BTreeMap<u64, Option<Arc<Blob>>>,
}

crate::kernel_local! {
    static SCOPE: RefCell<Option<ReadScope>> = const { RefCell::new(None) };
}

/// Run `read` (an answer's parse) with the ids in `handed` readable as blob handles; an id the answer never names is
/// released at once.
pub fn read_with<R>(blobs: &mut Blobs, handed: &[u64], read: impl FnOnce() -> R) -> R {
    let scope = ReadScope {
        handed: handed.iter().map(|id| (*id, None)).collect(),
    };
    let outer = SCOPE.with(|s| s.borrow_mut().replace(scope));
    let out = read();
    let scope = SCOPE.with(|s| core::mem::replace(&mut *s.borrow_mut(), outer));
    for (id, handle) in scope.map(|s| s.handed).unwrap_or_default() {
        match handle {
            Some(handle) => {
                blobs.live.insert(id, Arc::downgrade(&handle));
            }
            None => blobs.released.push(id),
        }
    }
    out
}

/// Whether an answer is being read with blobs handed.
pub(crate) fn reading() -> bool {
    SCOPE.with(|s| s.borrow().is_some())
}

/// The handle for `id`, one per id in an answer; an id the host never handed is refused.
pub(crate) fn handle(id: u64) -> Result<Arc<Blob>, String> {
    SCOPE.with(|s| {
        let mut slot = s.borrow_mut();
        let entry = slot
            .as_mut()
            .and_then(|scope| scope.handed.get_mut(&id))
            .ok_or_else(|| format!("{BLOB_MARKER} {id} was never handed to this kernel"))?;
        Ok(entry.get_or_insert_with(|| Arc::new(Blob { id })).clone())
    })
}

/// A framed body: `head` (JSON text naming each blob by its index) and `blobs` in index order.
pub fn frame(head: &[u8], blobs: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    let length = |len: usize| {
        u32::try_from(len)
            .map(u32::to_le_bytes)
            .map_err(|_| format!("a framed part of {len} bytes is over 4 GiB"))
    };
    let mut body =
        Vec::with_capacity(4 + head.len() + blobs.iter().map(|b| 4 + b.len()).sum::<usize>());
    body.extend_from_slice(&length(head.len())?);
    body.extend_from_slice(head);
    for blob in blobs {
        body.extend_from_slice(&length(blob.len())?);
        body.extend_from_slice(blob);
    }
    Ok(body)
}

/// The head and blobs of a framed body, borrowed from it.
pub fn unframe(body: &[u8]) -> Result<(&[u8], Vec<&[u8]>), String> {
    let mut rest = body;
    let mut take = || -> Result<Option<&[u8]>, String> {
        if rest.is_empty() {
            return Ok(None);
        }
        let (len, after) = rest
            .split_first_chunk::<4>()
            .ok_or("a framed body ends inside a length")?;
        let len = u32::from_le_bytes(*len) as usize;
        if after.len() < len {
            return Err(format!(
                "a framed part names {len} bytes and {} remain",
                after.len()
            ));
        }
        let (part, after) = after.split_at(len);
        rest = after;
        Ok(Some(part))
    };
    let head = take()?.ok_or("a framed body has no head")?;
    let mut blobs = Vec::new();
    while let Some(blob) = take()? {
        blobs.push(blob);
    }
    Ok((head, blobs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json_value::from_json_str;
    use crate::value::Value;

    fn read(blobs: &mut Blobs, handed: &[u64], text: &str) -> Result<Value, String> {
        read_with(blobs, handed, || {
            from_json_str::<Value>(text).map_err(|e| e.to_string())
        })
    }

    #[test]
    fn a_framed_body_splits_into_its_head_and_blobs() {
        let blobs = vec![
            b"{\"name\":\"g\"}".to_vec(),
            Vec::new(),
            "نص".as_bytes().to_vec(),
        ];
        let body = frame(b"{\"x\":{\"$blob\":0}}", &blobs).unwrap();
        let (head, parts) = unframe(&body).unwrap();
        assert_eq!(head, b"{\"x\":{\"$blob\":0}}");
        assert_eq!(parts, blobs.iter().map(Vec::as_slice).collect::<Vec<_>>());
    }

    #[test]
    fn control_a_body_cut_inside_a_part_is_refused() {
        let body = frame(b"{}", &[b"abc".to_vec()]).unwrap();
        assert!(
            unframe(&body[..body.len() - 1]).is_err(),
            "a part shorter than its length"
        );
        assert!(unframe(&body[..2]).is_err(), "a length cut short");
        assert!(unframe(&[]).is_err(), "no head");
    }

    #[test]
    fn a_handed_blob_marker_reads_as_a_handle_and_writes_back_as_its_marker() {
        let mut blobs = Blobs::new();
        let value = read(&mut blobs, &[7], r#"{"guest":"g","program":{"$blob":7}}"#).unwrap();
        let program = value.get_path("program").cloned().unwrap();
        assert!(
            matches!(&program, Value::Blob(b) if b.id() == 7),
            "{program:?}"
        );
        assert_eq!(
            serde_json::to_string(&value).unwrap(),
            r#"{"guest":"g","program":{"$blob":7}}"#
        );
        assert_eq!(blobs.held(), 1);
    }

    #[test]
    fn a_blob_id_never_handed_is_refused() {
        let mut blobs = Blobs::new();
        let refused = read(&mut blobs, &[7], r#"{"program":{"$blob":8}}"#).unwrap_err();
        assert!(refused.contains("never handed"), "{refused}");
        assert!(
            read(&mut blobs, &[], r#"{"$blob":1}"#).is_err(),
            "nothing handed"
        );
    }

    #[test]
    fn control_a_blob_marker_read_with_nothing_handed_is_an_ordinary_record() {
        let value: Value = from_json_str(r#"{"$blob":7}"#).unwrap();
        assert!(matches!(value, Value::Object(_)), "{value:?}");
    }

    #[test]
    fn control_a_marker_with_other_keys_is_refused_while_blobs_are_handed() {
        let mut blobs = Blobs::new();
        assert!(read(&mut blobs, &[7], r#"{"$blob":7,"value":1}"#).is_err());
    }

    #[test]
    fn a_blob_is_released_once_the_kernel_lets_go_of_it() {
        let mut blobs = Blobs::new();
        let value = read(&mut blobs, &[1, 2], r#"[{"$blob":1},{"$blob":1}]"#).unwrap();
        assert_eq!(
            blobs.take_released(),
            vec![2],
            "a handed id the answer never names is released at once"
        );
        let Value::Array(items) = &value else {
            panic!()
        };
        assert!(
            matches!((&items[0], &items[1]), (Value::Blob(a), Value::Blob(b)) if Arc::ptr_eq(a, b)),
            "one handle per id"
        );
        assert_eq!(items[0], items[1]);
        let kept = items[0].clone();
        drop(value);
        assert_eq!(
            blobs.take_released(),
            Vec::<u64>::new(),
            "control: a held handle is not released"
        );
        drop(kept);
        assert_eq!(blobs.take_released(), vec![1]);
        assert_eq!(blobs.held(), 0);
    }
}
