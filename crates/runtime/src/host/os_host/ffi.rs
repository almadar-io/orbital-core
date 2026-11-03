//! The native answerer of `ffi/call` (EXEC-METAL-2): dlopen a granted library, dlsym a granted symbol and call it
//! through libffi over the plain values and bytes the kernel sent. The kernel owns every `mem` block (EXEC-BOUND-2):
//! a pointer argument arrives as a copy of its block's bytes and a `mutptr` copy goes back with the answer. This is
//! the one module of the host that holds `unsafe`.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

#[cfg(feature = "ffi")]
use super::args::denied;
#[cfg(feature = "ffi")]
use super::completion::Completions;
use super::OsHost;
use crate::resumable::CallSite;
use orbital_core_base::os::{OsError, OsErrorKind, OsOp};
use orbital_core_base::value::Value;
#[cfg(feature = "ffi")]
use orbital_core_base::sync::Mutex;
#[cfg(feature = "ffi")]
use std::collections::HashMap;
#[cfg(feature = "ffi")]
use std::sync::Arc;

/// The libraries this program instance has loaded, by the library string its grant names.
#[derive(Clone)]
pub struct FfiTable {
    #[cfg(feature = "ffi")]
    libraries: Arc<Mutex<HashMap<String, Arc<libloading::Library>>>>,
}

impl Default for FfiTable {
    fn default() -> Self {
        Self {
            #[cfg(feature = "ffi")]
            libraries: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl FfiTable {
    pub(super) fn clear(&self) {
        #[cfg(feature = "ffi")]
        self.libraries.lock().clear();
    }
}

#[cfg(not(feature = "ffi"))]
fn unsupported() -> OsError {
    OsError::new(OsErrorKind::Unsupported, "ffi/call: this host has no ffi")
}

pub(super) fn answer(op: OsOp, args: &[Value], _key: Option<&str>, _site: Option<&CallSite>, host: &OsHost<'_>) -> Option<Result<Value, OsError>> {
    #[cfg(feature = "ffi")]
    {
        let table = host.handles.ffi.clone();
        Some(imp::prepare(op, args, host).and_then(|call| imp::execute(call, &table)))
    }
    #[cfg(not(feature = "ffi"))]
    {
        let _ = (op, args, host);
        Some(Err(unsupported()))
    }
}

/// `ffi/call` under a completion route: the grant is checked now and the foreign call runs on a thread of its own, so
/// a call that blocks holds the thread and not the lane.
pub(super) fn later(op: OsOp, args: &[Value], key: &str, site: &CallSite, host: &OsHost<'_>) -> Result<(), OsError> {
    #[cfg(feature = "ffi")]
    {
        let call = imp::prepare(op, args, host)?;
        let table = host.handles.ffi.clone();
        Completions::run_later(&host.handles.completions, key, site, move || imp::execute(call, &table)).map_err(|error| super::net::later_error(op, error, key))
    }
    #[cfg(not(feature = "ffi"))]
    {
        let _ = (op, args, key, site, host);
        Err(unsupported())
    }
}

#[cfg(feature = "ffi")]
mod imp {
    use super::*;
    use libffi::middle::{Arg, Cif, CodePtr, Type};
    use orbital_core_base::num_width::NumWidth;
    use orbital_core_base::os_grants::{parse_ffi_signature, FfiRet, FfiSignature, FfiType};
    use orbital_core_base::value::ValueMap;
    use std::ffi::c_void;
    use std::mem::discriminant;

    pub(super) struct Call {
        library: String,
        symbol: String,
        signature: FfiSignature,
        args: Vec<Value>,
    }

    fn invalid(message: impl Into<String>) -> OsError {
        OsError::new(OsErrorKind::Invalid, format!("ffi/call: {}", message.into()))
    }

    fn same_shape(granted: &FfiSignature, sent: &FfiSignature) -> bool {
        granted.params.len() == sent.params.len()
            && granted.params.iter().zip(&sent.params).all(|(a, b)| discriminant(a) == discriminant(b))
            && match (&granted.ret, &sent.ret) {
                (FfiRet::Void, FfiRet::Void) => true,
                (FfiRet::Scalar(a), FfiRet::Scalar(b)) => discriminant(a) == discriminant(b),
                _ => false,
            }
    }

    /// Re-check the grant (exact library and symbol) and read the ask: `[library, symbol, signature, args]`.
    pub(super) fn prepare(op: OsOp, args: &[Value], host: &OsHost<'_>) -> Result<Call, OsError> {
        let (Some(Value::String(library)), Some(Value::String(symbol)), Some(Value::String(text)), Some(Value::Array(values))) =
            (args.first(), args.get(1), args.get(2), args.get(3))
        else {
            return Err(invalid("expects a library, a symbol, a signature and an argument list"));
        };
        let granted = host
            .grants
            .ffi_signature(library, symbol)
            .ok_or_else(|| denied(op, &format!("no ffi grant covers {library} {symbol}")))?;
        let sent = parse_ffi_signature(text).map_err(|message| invalid(format!("the signature is malformed: {message}")))?;
        if !same_shape(&granted, &sent) {
            return Err(invalid("the signature is not the one granted"));
        }
        if values.len() != granted.params.len() {
            return Err(invalid(format!("the signature takes {} arguments, got {}", granted.params.len(), values.len())));
        }
        Ok(Call { library: library.to_string(), symbol: symbol.to_string(), signature: granted, args: values.to_vec() })
    }

    enum Cell {
        I8(i8),
        I16(i16),
        I32(i32),
        I64(i64),
        U8(u8),
        U16(u16),
        U32(u32),
        U64(u64),
        F32(f32),
        F64(f64),
        Ptr(*mut c_void),
        Buffer(usize),
    }

    impl Cell {
        fn arg(&self) -> Arg {
            match self {
                Cell::I8(v) => Arg::new(v),
                Cell::I16(v) => Arg::new(v),
                Cell::I32(v) => Arg::new(v),
                Cell::I64(v) => Arg::new(v),
                Cell::U8(v) => Arg::new(v),
                Cell::U16(v) => Arg::new(v),
                Cell::U32(v) => Arg::new(v),
                Cell::U64(v) => Arg::new(v),
                Cell::F32(v) => Arg::new(v),
                Cell::F64(v) => Arg::new(v),
                Cell::Ptr(v) => Arg::new(v),
                Cell::Buffer(_) => unreachable!("a buffer cell is resolved to a pointer before the call"),
            }
        }
    }

    fn ctype(ty: &FfiType) -> Type {
        match ty {
            FfiType::Num(NumWidth::I8) => Type::i8(),
            FfiType::Num(NumWidth::I16) => Type::i16(),
            FfiType::Num(NumWidth::I32) => Type::i32(),
            FfiType::Num(NumWidth::I64) => Type::i64(),
            FfiType::Num(NumWidth::U8) => Type::u8(),
            FfiType::Num(NumWidth::U16) => Type::u16(),
            FfiType::Num(NumWidth::U32) => Type::u32(),
            FfiType::Num(NumWidth::U64) => Type::u64(),
            FfiType::Num(NumWidth::F32) => Type::f32(),
            FfiType::Num(NumWidth::F64) => Type::f64(),
            FfiType::Ptr | FfiType::MutPtr => Type::pointer(),
        }
    }

    /// The argument as the C scalar its width names; a value outside the width is refused, never wrapped.
    fn scalar(width: NumWidth, value: &Value, place: usize) -> Result<Cell, OsError> {
        if !width.fits(value) {
            return Err(invalid(format!("argument {place} does not fit {width}")));
        }
        let int = || value.as_int().unwrap_or_default();
        let float = || value.as_number().unwrap_or_default();
        Ok(match width {
            NumWidth::I8 => Cell::I8(int() as i8),
            NumWidth::I16 => Cell::I16(int() as i16),
            NumWidth::I32 => Cell::I32(int() as i32),
            NumWidth::I64 => Cell::I64(int()),
            NumWidth::U8 => Cell::U8(int() as u8),
            NumWidth::U16 => Cell::U16(int() as u16),
            NumWidth::U32 => Cell::U32(int() as u32),
            NumWidth::U64 => Cell::U64(int() as u64),
            NumWidth::F32 => Cell::F32(float() as f32),
            NumWidth::F64 => Cell::F64(float()),
        })
    }

    fn buffer(value: &Value, place: usize) -> Result<Vec<u8>, OsError> {
        value.as_bytes().map(<[u8]>::to_vec).ok_or_else(|| invalid(format!("argument {place} must be bytes")))
    }

    fn library(table: &FfiTable, name: &str) -> Result<Arc<libloading::Library>, OsError> {
        let mut libraries = table.libraries.lock();
        if let Some(loaded) = libraries.get(name) {
            return Ok(loaded.clone());
        }
        // SAFETY: loading a library runs its initialisers. The program named this exact library string in its `ffi`
        // grant, which is the author's assertion that loading it is acceptable; the string is passed verbatim.
        let loaded = unsafe { libloading::Library::new(name) }
            .map_err(|error| OsError::new(OsErrorKind::NotFound, format!("ffi/call: {name} cannot be loaded: {error}")))?;
        let loaded = Arc::new(loaded);
        libraries.insert(name.to_string(), loaded.clone());
        Ok(loaded)
    }

    fn invoke<R>(cif: &Cif, code: CodePtr, args: &[Arg]) -> R {
        // SAFETY: `code` is the address of a symbol the program's grant names with the signature `cif` was built
        // from, `args` are one cell per declared parameter holding that parameter's C type (pointers address buffers
        // that outlive the call), and every caller picks `R` from the declared return type (word-sized for integers
        // narrower than a word, because libffi widens those results).
        unsafe { cif.call::<R>(code, args) }
    }

    pub(super) fn execute(call: Call, table: &FfiTable) -> Result<Value, OsError> {
        let Call { library: name, symbol, signature, args } = call;
        let loaded = library(table, &name)?;
        // SAFETY: the symbol is read as a bare address (`*mut c_void`) and only used while `loaded` is alive; the
        // lookup itself has no side effect.
        let address = unsafe { loaded.get::<*mut c_void>(symbol.as_bytes()) }
            .map(|found| *found)
            .map_err(|error| OsError::new(OsErrorKind::NotFound, format!("ffi/call: {symbol} is not in {name}: {error}")))?;
        let mut buffers: Vec<Vec<u8>> = Vec::new();
        let mut writable: Vec<usize> = Vec::new();
        let mut cells = Vec::with_capacity(args.len());
        for (index, (ty, value)) in signature.params.iter().zip(&args).enumerate() {
            let place = index + 1;
            cells.push(match ty {
                FfiType::Num(width) => scalar(*width, value, place)?,
                FfiType::Ptr | FfiType::MutPtr => {
                    buffers.push(buffer(value, place)?);
                    if matches!(ty, FfiType::MutPtr) {
                        writable.push(buffers.len() - 1);
                    }
                    Cell::Buffer(buffers.len() - 1)
                }
            });
        }
        for cell in &mut cells {
            if let Cell::Buffer(slot) = *cell {
                *cell = Cell::Ptr(buffers[slot].as_mut_ptr().cast());
            }
        }
        let ret = match &signature.ret {
            FfiRet::Void => Type::void(),
            FfiRet::Scalar(FfiType::Ptr | FfiType::MutPtr) => return Err(invalid("a pointer cannot be returned")),
            FfiRet::Scalar(ty) => ctype(ty),
        };
        let cif = Cif::new(signature.params.iter().map(ctype), ret);
        let slots: Vec<Arg> = cells.iter().map(Cell::arg).collect();
        let code = CodePtr::from_ptr(address);
        let wide = |n: i64| Value::Int(n);
        let value = match &signature.ret {
            FfiRet::Void => {
                invoke::<()>(&cif, code, &slots);
                Value::Null
            }
            FfiRet::Scalar(ty) => match ty {
                FfiType::Num(NumWidth::I8) => wide(i64::from(invoke::<isize>(&cif, code, &slots) as i8)),
                FfiType::Num(NumWidth::I16) => wide(i64::from(invoke::<isize>(&cif, code, &slots) as i16)),
                FfiType::Num(NumWidth::I32) => wide(i64::from(invoke::<isize>(&cif, code, &slots) as i32)),
                FfiType::Num(NumWidth::I64) => wide(invoke::<i64>(&cif, code, &slots)),
                FfiType::Num(NumWidth::U8) => wide(i64::from(invoke::<usize>(&cif, code, &slots) as u8)),
                FfiType::Num(NumWidth::U16) => wide(i64::from(invoke::<usize>(&cif, code, &slots) as u16)),
                FfiType::Num(NumWidth::U32) => wide(i64::from(invoke::<usize>(&cif, code, &slots) as u32)),
                FfiType::Num(NumWidth::U64) => {
                    let raw = invoke::<u64>(&cif, code, &slots);
                    wide(i64::try_from(raw).map_err(|_| OsError::new(OsErrorKind::Io, "ffi/call: the u64 result exceeds the int range"))?)
                }
                FfiType::Num(NumWidth::F32) => Value::Number(f64::from(invoke::<f32>(&cif, code, &slots))),
                FfiType::Num(NumWidth::F64) => Value::Number(invoke::<f64>(&cif, code, &slots)),
                FfiType::Ptr | FfiType::MutPtr => return Err(invalid("a pointer cannot be returned")),
            },
        };
        let returned: Vec<Value> = writable.iter().map(|slot| Value::bytes(buffers[*slot].clone())).collect();
        Ok(Value::object(ValueMap::from_iter([("value".into(), value), ("buffers".into(), Value::array(returned))])))
    }
}

#[cfg(all(test, feature = "ffi"))]
mod tests {
    use super::super::*;
    use orbital_core_base::os_grants::FfiGrant;

    #[cfg(target_os = "macos")]
    const MATH: &str = "/usr/lib/libSystem.B.dylib";
    #[cfg(target_os = "macos")]
    const LIBC: &str = "/usr/lib/libSystem.B.dylib";
    #[cfg(target_os = "linux")]
    const MATH: &str = "libm.so.6";
    #[cfg(target_os = "linux")]
    const LIBC: &str = "libc.so.6";

    fn grant(library: &str, symbol: &str, signature: &str) -> FfiGrant {
        FfiGrant { library: library.to_string(), symbol: symbol.to_string(), signature: signature.to_string() }
    }

    fn call(grants: &OsGrants, library: &str, symbol: &str, signature: &str, args: Vec<Value>) -> Result<Value, OsError> {
        let handles = OsHandles::default();
        let on_event = |_: Option<&CallSite>, _: Value| {};
        let ask = vec![Value::string(library.to_string()), Value::string(symbol.to_string()), Value::string(signature.to_string()), Value::array(args)];
        let request = IoRequest::Os { op: OsOp::FfiCall, args: ask, key: None, site: None, principal: None, orbital: None };
        match answer_os(Some(&OsHost { grants, handles: &handles, on_event: &on_event, secrets: None }), &request) {
            IoAnswer::Os(answer) => answer.map(Secret::into_inner),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_granted_c_function_is_called_and_its_result_returned() {
        let mut grants = OsGrants::default();
        grants.ffi.push(grant(MATH, "cos", "(f64) -> f64"));
        let answer = call(&grants, MATH, "cos", "(f64) -> f64", vec![Value::Number(0.0)]).expect("cos answers");
        assert_eq!(answer.get("value"), Some(&Value::Number(1.0)));
        assert_eq!(answer.get("buffers"), Some(&Value::array(Vec::<Value>::new())));
    }

    #[test]
    fn a_mutptr_buffer_comes_back_as_the_c_function_left_it() {
        let mut grants = OsGrants::default();
        grants.ffi.push(grant(LIBC, "bzero", "(mutptr, u64) -> void"));
        let answer = call(&grants, LIBC, "bzero", "(mutptr, u64) -> void", vec![Value::bytes(vec![1u8, 2, 3, 4]), Value::Int(4)]).expect("bzero answers");
        assert_eq!(answer.get("value"), Some(&Value::Null));
        assert_eq!(answer.get("buffers"), Some(&Value::array(vec![Value::bytes(vec![0u8; 4])])));
    }

    #[test]
    fn a_symbol_the_grant_does_not_name_is_denied() {
        let mut grants = OsGrants::default();
        grants.ffi.push(grant(MATH, "cos", "(f64) -> f64"));
        let error = call(&grants, MATH, "sin", "(f64) -> f64", vec![Value::Number(0.0)]).expect_err("sin is not granted");
        assert_eq!(error.kind, OsErrorKind::Denied);
        let control = call(&grants, MATH, "cos", "(f64) -> f64", vec![Value::Number(0.0)]);
        assert!(control.is_ok(), "control: the granted symbol answers");
    }

    #[test]
    fn a_library_that_cannot_be_loaded_is_not_found() {
        let mut grants = OsGrants::default();
        grants.ffi.push(grant("/nonexistent/libnope.so", "f", "() -> void"));
        let error = call(&grants, "/nonexistent/libnope.so", "f", "() -> void", Vec::new()).expect_err("no such library");
        assert_eq!(error.kind, OsErrorKind::NotFound);
    }
}
