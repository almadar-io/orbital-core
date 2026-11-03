//! The `ffi` grant: a foreign library, a symbol in it, and the C signature the program may call it with.

use super::*;

/// One callable foreign function: `library` is passed to the host's loader verbatim, `signature` is the C signature
/// `(<type>, ...) -> <ret>` (see [`parse_ffi_signature`]).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FfiGrant {
    pub library: String,
    pub symbol: String,
    pub signature: String,
}

/// The most parameters a granted signature may have.
pub const FFI_MAX_PARAMS: usize = 8;

/// A C parameter type: a fixed-width scalar, or a `mem` borrow passed by pointer (`ptr` read-only, `mutptr` writable).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FfiType {
    Num(crate::num_width::NumWidth),
    Ptr,
    MutPtr,
}

impl FfiType {
    /// The type spelled `name`: a width name, `ptr` or `mutptr`.
    pub fn from_name(name: &str) -> Option<FfiType> {
        match name {
            "ptr" => Some(FfiType::Ptr),
            "mutptr" => Some(FfiType::MutPtr),
            width => crate::num_width::NumWidth::from_name(width).map(FfiType::Num),
        }
    }

    /// The grant grammar's spelling.
    pub fn name(self) -> &'static str {
        match self {
            FfiType::Num(width) => width.name(),
            FfiType::Ptr => "ptr",
            FfiType::MutPtr => "mutptr",
        }
    }

    /// Whether the type is a pointer to borrowed memory.
    pub fn is_pointer(self) -> bool {
        matches!(self, FfiType::Ptr | FfiType::MutPtr)
    }
}

/// What a foreign function returns: nothing, or a scalar (never a pointer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FfiRet {
    Void,
    Scalar(FfiType),
}

/// A parsed C signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfiSignature {
    pub params: Vec<FfiType>,
    pub ret: FfiRet,
}

/// Parses `"(" [type ("," type)*] ")" "->" ret`, whitespace allowed around the parentheses, commas and arrow. At most
/// [`FFI_MAX_PARAMS`] parameters; a return is a scalar type or `void`, never `ptr` or `mutptr`.
pub fn parse_ffi_signature(text: &str) -> Result<FfiSignature, String> {
    let rest = text.trim().strip_prefix('(').ok_or_else(|| format!("{text:?} does not start with `(`"))?;
    let (inside, after) = rest.split_once(')').ok_or_else(|| format!("{text:?} has no closing `)`"))?;
    let mut params = Vec::new();
    if !inside.trim().is_empty() {
        for piece in inside.split(',') {
            let name = piece.trim();
            let kind = FfiType::from_name(name).ok_or_else(|| {
                if name.is_empty() {
                    "an empty parameter type".to_string()
                } else {
                    format!("{name:?} is not a parameter type (i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 ptr mutptr)")
                }
            })?;
            params.push(kind);
        }
    }
    if params.len() > FFI_MAX_PARAMS {
        return Err(format!("{} parameters, at most {FFI_MAX_PARAMS}", params.len()));
    }
    let ret = after.trim().strip_prefix("->").ok_or_else(|| format!("{text:?} has no `->` return type after the parameters"))?.trim();
    let ret = match ret {
        "void" => FfiRet::Void,
        name => match FfiType::from_name(name) {
            Some(kind) if kind.is_pointer() => return Err(format!("a `{name}` return is not supported: return a scalar or void")),
            Some(kind) => FfiRet::Scalar(kind),
            None => return Err(format!("{name:?} is not a return type (a scalar type or void)")),
        },
    };
    Ok(FfiSignature { params, ret })
}

impl OsGrants {
    /// Whether the symbol of the library is granted, by exact library string and exact symbol name.
    pub fn ffi_allows(&self, library: &str, symbol: &str) -> bool {
        self.ffi.iter().any(|g| g.library == library && g.symbol == symbol)
    }

    /// The signature the grant gives the symbol; `None` when it is not granted or its signature does not parse.
    pub fn ffi_signature(&self, library: &str, symbol: &str) -> Option<FfiSignature> {
        self.ffi.iter().find(|g| g.library == library && g.symbol == symbol).and_then(|g| parse_ffi_signature(&g.signature).ok())
    }
}
