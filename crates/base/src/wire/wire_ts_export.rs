#[cfg(not(feature = "std"))]
use std::prelude::v1::*;
use crate::client_effect::ClientEffect;
use crate::client_effect::HostAskCompletion;
use crate::client_effect::NavigateOptions;
use crate::row_set::RowCompareOp;
use crate::row_set::RowPredicate;
use crate::dispatch_mode::DispatchMode;
use crate::theme::ResolvedTheme;
use crate::value::Value;
use std::path::PathBuf;
use ts_rs::Config;
use ts_rs::TypeVisitor;
use ts_rs::TS;


/// Declares a type whose TypeScript form is written here: `$name` is its exported name and
/// `$decl` its right-hand side.
macro_rules! declared_by_hand {
    ($ty:ty, $name:literal, $decl:literal $(, [$($dep:ty),*])?) => {
        impl TS for $ty {
            type WithoutGenerics = Self;
            type OptionInnerType = Self;
            fn name(_: &Config) -> String {
                $name.to_string()
            }
            fn inline(_: &Config) -> String {
                $name.to_string()
            }
            fn decl(_: &Config) -> String {
                format!("type {} = {}", $name, $decl)
            }
            fn decl_concrete(cfg: &Config) -> String {
                <Self as TS>::decl(cfg)
            }
            fn output_path() -> Option<PathBuf> {
                Some(PathBuf::from(concat!($name, ".ts")))
            }
            fn visit_dependencies(_v: &mut impl TypeVisitor)
            where
                Self: 'static,
            {
                $($(_v.visit::<$dep>();)*)?
            }
        }
    };
}


// The evaluator value serializes as plain JSON; `@almadar/core` calls that `JsonValue`.
impl TS for Value {
    type WithoutGenerics = Self;
    type OptionInnerType = Self;
    fn name(_: &Config) -> String {
        "JsonValue".to_string()
    }
    fn inline(_: &Config) -> String {
        "JsonValue".to_string()
    }
}

declared_by_hand!(
    ClientEffect,
    "ClientEffect",
    "[\"render-ui\", string, JsonObject | null] | [\"navigate\", string] | [\"navigate\", string, { [key in string]: string } | null] | [\"navigate\", string, { [key in string]: string } | null, NavigateOptions] | [\"navigate-back\"] | [\"host-ask\", string, Array<JsonValue>] | [\"host-ask\", string, Array<JsonValue>, HostAskCompletion]",
    [NavigateOptions, HostAskCompletion]
);
declared_by_hand!(
    DispatchMode,
    "DispatchMode",
    "\"persistedAwaited\" | \"hybridClientOnly\" | \"runtimeOptimistic\""
);
declared_by_hand!(
    ResolvedTheme,
    "ResolvedTheme",
    "{ key: string, base: string, mode: \"light\" | \"dark\", tokens: JsonObject }"
);
declared_by_hand!(
    RowPredicate,
    "RowPredicate",
    "{ kind: \"compare\", field: string, op: RowCompareOp, value: FieldValue } | { kind: \"member\", field: string, values: Array<FieldValue> } | { kind: \"hasElement\", field: string, value: FieldValue } | { kind: \"contains\", field: string, text: string, caseSensitive: boolean } | { kind: \"isNull\", field: string, isNull: boolean } | { kind: \"all\", of: Array<RowPredicate> } | { kind: \"any\", of: Array<RowPredicate> } | { kind: \"not\", of: RowPredicate } | { kind: \"const\", value: boolean }",
    [RowCompareOp]
);
