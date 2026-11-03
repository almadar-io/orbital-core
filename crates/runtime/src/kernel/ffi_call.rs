//! `ffi/call` (EXEC-BOUND-2, EXEC-METAL-2): the kernel checks the call against the signature its orbital was granted
//! and reads the bytes of every pointer argument out of its own memory. The host only calls C over plain values and
//! bytes; the bytes a `mutptr` comes back with are written into its block here, staged with the step like `mem/write`.

use super::*;
use orbital_core_base::os::OsOp;
use orbital_core_base::os_grants::{FfiRet, FfiSignature, FfiType};

fn invalid(message: impl Into<String>) -> OsError {
    OsError::new(OsErrorKind::Invalid, message)
}

/// The granted signature in the grammar the host parses: `(f64, u64) -> void`.
fn signature_text(signature: &FfiSignature) -> String {
    let params: Vec<&str> = signature.params.iter().map(|ty| ty.name()).collect();
    let ret = match &signature.ret {
        FfiRet::Void => "void",
        FfiRet::Scalar(ty) => ty.name(),
    };
    format!("({}) -> {ret}", params.join(", "))
}

/// The recorded bytes of a `mutptr` buffer: a bytes value, or the standard base64 text a journaled answer carries.
fn buffer_bytes(value: &Value) -> Option<Vec<u8>> {
    match value {
        Value::Bytes(data) => Some(data.to_vec()),
        Value::String(text) => orbital_core_base::codec::base64_decode(text).ok(),
        _ => None,
    }
}

impl RuntimeKernel {
    /// Answer an `ffi/call`: validate it against the granted signature, ask the host with plain values and bytes, and
    /// write each `mutptr` buffer back. `None` for an op outside the family. The ask is the journaled host ask, so a
    /// replayed step takes the recorded answer and never calls C again. A keyed call the host defers answers later as
    /// a completion carrying its `buffers`; nothing is written back for it.
    pub(super) fn ffi_os(&self, op: OsOp, args: &[Value], host_key: Option<&str>, principal: Option<&str>, orbital: Option<&str>) -> Option<Result<Value, OsError>> {
        if op != OsOp::FfiCall {
            return None;
        }
        let run = || -> Result<Value, OsError> {
            let (Some(Value::String(library)), Some(Value::String(symbol))) = (args.first(), args.get(1)) else {
                return Err(invalid("the library and the symbol must be text"));
            };
            let signature = self
                .grants
                .os_grants
                .read()
                .for_orbital(orbital)
                .ffi_signature(library, symbol)
                .ok_or_else(|| OsError::new(OsErrorKind::Denied, format!("no ffi grant covers {library} {symbol}")))?;
            let Some(Value::Array(values)) = args.get(2) else {
                return Err(invalid("the arguments must be a list"));
            };
            let (marshalled, mutptrs) = self.ffi_marshal(&signature, values)?;
            let ask = [
                Value::string(library.to_string()),
                Value::string(symbol.to_string()),
                Value::string(signature_text(&signature)),
                Value::array(marshalled),
            ];
            let answer = self.io.os(op, &ask, host_key, principal, orbital)?;
            if mutptrs.is_empty() {
                return Ok(answer);
            }
            let Some(Value::Array(buffers)) = answer.get("buffers") else {
                return Ok(answer);
            };
            if buffers.len() != mutptrs.len() {
                return Err(OsError::new(OsErrorKind::Io, "the host returned the wrong number of buffers"));
            }
            for (borrow, buffer) in mutptrs.iter().zip(buffers.iter()) {
                let bytes = buffer_bytes(buffer).ok_or_else(|| OsError::new(OsErrorKind::Io, "a returned buffer is not bytes"))?;
                self.ffi_write_back(borrow, &bytes)?;
            }
            Ok(answer)
        };
        Some(run().map_err(|error| OsError::new(error.kind, format!("{}: {}", op.name(), error.message))))
    }

    /// The arguments as the host receives them, and the borrows of the `mutptr` parameters in order.
    fn ffi_marshal(&self, signature: &FfiSignature, values: &[Value]) -> Result<(Vec<Value>, Vec<Value>), OsError> {
        if values.len() != signature.params.len() {
            return Err(invalid(format!("the signature takes {} arguments, got {}", signature.params.len(), values.len())));
        }
        let mut marshalled = Vec::with_capacity(values.len());
        let mut mutptrs = Vec::new();
        for (index, (ty, value)) in signature.params.iter().zip(values).enumerate() {
            let place = index + 1;
            match ty {
                FfiType::Num(width) => {
                    if !width.fits(value) {
                        return Err(invalid(format!("argument {place} does not fit {width}")));
                    }
                    marshalled.push(match (width.is_float(), value.as_int()) {
                        (false, Some(number)) => Value::Int(number),
                        _ => Value::Number(value.as_number().unwrap_or_default()),
                    });
                }
                FfiType::Ptr | FfiType::MutPtr => {
                    let exclusive = matches!(ty, FfiType::MutPtr);
                    let bytes = self.ffi_borrow_bytes(value, exclusive).map_err(|error| invalid(format!("argument {place}: {}", error.message)))?;
                    if exclusive {
                        mutptrs.push(value.clone());
                    }
                    marshalled.push(Value::bytes(bytes));
                }
            }
        }
        Ok((marshalled, mutptrs))
    }
}
