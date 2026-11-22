//! EXEC-DIST-16b: an answer framed for a browser host, each SExpr-typed emit payload value carried as a blob.

use super::RuntimeKernel;
use crate::effects::response::EventResponse;
use orbital_core_base::blobs::{frame, BLOB_MARKER};
use orbital_core_base::json_value::{js_json_string, value_json_string_with_defs};
use orbital_core_base::value::{Value, ValueMap};

impl RuntimeKernel {
    /// `response` as a framed body, and its head: the head names each top-level emit payload field its emitter declares
    /// `SExpr` as `{ "$blob": n }`, and the value's JSON text is blob `n`. Decoded, the body is the plain answer exactly.
    pub fn framed_answer(
        &self,
        response: &EventResponse,
    ) -> Result<(EventResponse, Vec<u8>), String> {
        let mut head = response.clone();
        let mut blobs = Vec::new();
        let definitions = self.registry.trait_definitions.read();
        for emitted in &mut head.emitted_events {
            let Some(trait_name) = emitted.source.as_ref().and_then(|s| s.r#trait.as_deref())
            else {
                continue;
            };
            let Some(fields) = definitions
                .get(trait_name)
                .and_then(|def| def.emits.as_ref())
                .and_then(|emits| emits.iter().find(|e| e.event == emitted.event))
                .and_then(|contract| contract.payload_schema.as_ref())
            else {
                continue;
            };
            let Some(Value::Object(payload)) = emitted.payload.as_mut() else {
                continue;
            };
            for field in fields.iter().filter(|f| f.field_type == "SExpr") {
                let Some(value) = payload
                    .get_mut(field.name.as_str())
                    .filter(|v| !v.is_null())
                else {
                    continue;
                };
                blobs.push(value_json_string_with_defs(value).into_bytes());
                let mut marker = ValueMap::default();
                marker.insert(BLOB_MARKER.into(), Value::Int(blobs.len() as i64 - 1));
                *value = Value::object(marker);
            }
        }
        drop(definitions);
        let body = frame(js_json_string(&head).as_bytes(), &blobs)?;
        Ok((head, body))
    }
}
