//! S-Expression Operators
//!
//! This module implements all operators for S-expression evaluation:
//! - Arithmetic: +, -, *, /, %
//! - Comparison: =, !=, <, >, <=, >=
//! - Logic: and, or, not, if
//! - Array: count, first, last, nth, includes, map, filter, find
//! - String: str/concat, upper, lower, trim, substring, split, join
//! - Math: min, max, clamp, abs, floor, ceil, round
//! - Control flow: do, when, let
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::context::BindingContext;
use orbital_core_base::error::{EvalError, OrbitalError};
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_base::value::Value;
use orbital_core_base::value::ValueMap;

/// Result type for operator evaluation
pub type EvalResult = Result<Value, OrbitalError>;

/// Trait for operator implementations
pub trait Operator {
    /// Evaluate this operator with the given arguments and context
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult;
}

pub mod arg_types;
pub mod kind;
pub mod macros;
pub use arg_types::operator_signature;
pub use kind::{OperatorKind, TypePredicate};
pub mod anim;
pub mod array;
pub mod bit;
pub mod bytes;
pub mod cast;
pub mod csv;
pub mod digest;
pub mod ease;
pub mod format;
pub mod geo;
pub mod grid;
pub mod i18n;
pub mod json;
pub mod logic;
pub mod math;
pub mod noise;
pub mod object;
pub mod os_path;
pub mod path;
pub mod prob;
pub mod row;
pub mod string;
pub mod system;
pub mod tree;
pub mod util;
pub mod vector;
pub mod xml;

pub use anim::*;
pub use array::*;
pub use bit::*;
pub use bytes::*;
pub use cast::*;
pub use csv::*;
pub use digest::*;
pub use ease::*;
pub use format::*;
pub use geo::*;
pub use grid::*;
pub use i18n::*;
pub use json::*;
pub use logic::*;
pub use math::*;
pub use noise::*;
pub use object::*;
pub use os_path::*;
pub use path::*;
pub use prob::*;
pub use row::*;
pub use string::*;
pub use system::*;
pub use tree::*;
pub use vector::*;
pub use xml::*;

/// The names a raw-body callback of operator `name` binds (see [`OperatorKind::raw_callback_params`]).
pub fn raw_callback_params(name: &str) -> Option<&'static [&'static str]> {
    get_operator(name).and_then(|kind| kind.raw_callback_params())
}

/// The position of the `number` index among the parameters of operator `name`'s callback (see
/// [`OperatorKind::callback_index_position`]).
pub fn callback_index_position(name: &str) -> Option<usize> {
    get_operator(name).and_then(|kind| kind.callback_index_position())
}

/// Declares the dispatch table once: [`get_operator`] (name to operator, a `match` with no vtable or per-lookup
/// allocation) and [`DISPATCHED_NAMES`] (every name it answers) are both generated from the same arms.
macro_rules! operator_dispatch {
    ($( $($name:literal)|+ => $kind:expr ),+ $(,)?) => {
        /// Every operator name [`get_operator`] dispatches.
        pub const DISPATCHED_NAMES: &[&str] = &[ $( $($name),+ ),+ ];

        /// Get an operator by name.
        ///
        /// Returns an [`OperatorKind`] (a stack value enum) rather than a boxed trait object.
        pub fn get_operator(name: &str) -> Option<OperatorKind> {
            use crate::evaluator::operators::kind::OperatorKind as K;
            match name {
                $( $($name)|+ => Some($kind), )+
                _ => None,
            }
        }
    };
}

operator_dispatch! {
        // Arithmetic
        "+" => K::AddOp,
        "-" => K::SubOp,
        "*" => K::MulOp,
        "/" => K::DivOp,
        "%" => K::ModOp,

        // Comparison
        "=" | "==" => K::EqOp,
        "!=" => K::NeOp,
        "<" => K::LtOp,
        ">" => K::GtOp,
        "<=" => K::LeOp,
        ">=" => K::GeOp,

        // Logic
        "and" => K::AndOp,
        "or" => K::OrOp,
        "default" => K::DefaultOp,
        "not" => K::NotOp,
        "if" => K::IfOp,

        // Array
        "array/len" => K::ArrayLenOp,
        "array/first" => K::FirstOp,
        "array/last" => K::LastOp,
        "array/nth" => K::NthOp,
        "array/includes" => K::IncludesOp,
        "array/map" => K::MapOp,
        "array/filter" => K::FilterOp,
        "array/find" => K::FindOp,

        // Phase 2.1: Array Essentials
        "array/slice" => K::ArraySliceOp,
        "array/concat" => K::ArrayConcatOp,
        "array/append" => K::ArrayAppendOp,
        "array/sort" => K::ArraySortOp,
        "array/unique" => K::ArrayUniqueOp,
        "array/indexOf" => K::ArrayIndexOfOp,
        "array/every" => K::ArrayEveryOp,
        "array/some" => K::ArraySomeOp,
        "array/sum" => K::ArraySumOp,
        "array/cosine" => K::ArrayCosineOp,
        "array/nearest" => K::ArrayNearestOp,

        // String (bare "concat" is deliberately NOT an alias — ambiguous
        // with array/concat; see the dispatch-site check in evaluator/mod.rs)
        "str/concat" => K::ConcatOp,
        "str/upper" => K::UpperOp,
        "str/lower" => K::LowerOp,
        "str/trim" => K::TrimOp,
        "str/split" => K::SplitOp,
        "str/join" => K::JoinOp,

        // Explicit cast
        "as" => K::AsOp,
        "typed" => K::TypedOp,

        // JSON
        "json/parse" => K::JsonParseOp,
        "json/stringify" => K::JsonStringifyOp,
        "json/valid" => K::JsonValidOp,
        "json/nodes" => K::JsonNodesOp,
        "tree/index" => K::TreeIndexOp,

        // Structural fold over a recursive type; the kernel's one row predicate evaluator
        "tree/fold" => K::TreeFoldOp,
        "row/matches" => K::RowMatchesOp,

        // CSV
        "csv/parse" => K::CsvParseOp,

        // XML
        "xml/parse" => K::XmlParseOp,
        "xml/valid" => K::XmlValidOp,

        // Path building (for dynamic field paths in set effect)
        "path" => K::PathOp,

        // Phase 2.2: String Operations
        "str/len" => K::StrLenOp,
        "str/includes" => K::StrIncludesOp,
        "str/startsWith" => K::StrStartsWithOp,
        "str/endsWith" => K::StrEndsWithOp,
        "str/replace" => K::StrReplaceOp,
        "str/replaceAll" => K::StrReplaceAllOp,
        "str/replace-pattern" => K::StrReplacePatternOp,
        "str/template" => K::StrTemplateOp,

        // Math
        "math/min" => K::MinOp,
        "math/max" => K::MaxOp,
        "math/clamp" => K::ClampOp,
        "math/abs" => K::AbsOp,
        "math/floor" => K::FloorOp,
        "math/ceil" => K::CeilOp,
        "math/round" => K::RoundOp,

        // Phase 2.4: Math Operations
        "math/pow" => K::MathPowOp,
        "math/sqrt" => K::MathSqrtOp,
        "math/log10" => K::MathLog10Op,
        "math/ln" => K::MathLnOp,

        // Control flow
        "do" => K::DoOp,
        "when" => K::WhenOp,
        "let" => K::LetOp,

        // Lambda (Phase 1)
        "fn" => K::FnOp,
        "quote" => K::QuoteOp,
        "quasiquote" => K::QuasiquoteOp,
        "unquote" => K::UnquoteOp,

        // Reduce (Phase 1)
        "array/reduce" => K::ReduceOp,

        // Object (Phase 1)
        "object/keys" => K::ObjectKeysOp,
        "object/values" => K::ObjectValuesOp,
        "object/get" => K::ObjectGetOp,

        // Time (Phase 1)
        "time/now" => K::TimeNowOp,

        // Phase 2.3: Object Operations
        "object/entries" => K::ObjectEntriesOp,
        "object/set" => K::ObjectSetOp,
        "object/has" => K::ObjectHasOp,
        "object/merge" => K::ObjectMergeOp,

        // Phase 2.5: Time Operations
        "time/today" => K::TimeTodayOp,
        "time/add" => K::TimeAddOp,
        "time/subtract" => K::TimeSubtractOp,
        "time/diff" => K::TimeDiffOp,
        "time/isBefore" => K::TimeIsBeforeOp,
        "time/isAfter" => K::TimeIsAfterOp,
        "time/parse" => K::TimeParseOp,
        "time/valid" => K::TimeValidOp,
        "time/format" => K::TimeFormatOp,

        // Phase 2.6: Validation Operations
        "validate/required" => K::ValidateRequiredOp,
        "validate/string" => K::ValidateStringOp,
        "validate/number" => K::ValidateNumberOp,
        "validate/boolean" => K::ValidateBooleanOp,
        "validate/array" => K::ValidateArrayOp,
        "validate/object" => K::ValidateObjectOp,
        "validate/email" => K::ValidateEmailOp,
        "validate/url" => K::ValidateUrlOp,
        "validate/minLength" => K::ValidateMinLengthOp,
        "validate/maxLength" => K::ValidateMaxLengthOp,
        "validate/pattern" => K::ValidatePatternOp,

        // Phase 3.1: Array Advanced
        "array/prepend" => K::ArrayPrependOp,
        "array/insert" => K::ArrayInsertOp,
        "array/remove" => K::ArrayRemoveOp,
        "array/reverse" => K::ArrayReverseOp,
        "array/flatten" => K::ArrayFlattenOp,
        "array/zip" => K::ArrayZipOp,
        "array/findIndex" => K::ArrayFindIndexOp,
        "array/avg" => K::ArrayAvgOp,
        "array/groupBy" => K::ArrayGroupByOp,
        "array/take" => K::ArrayTakeOp,
        "array/drop" => K::ArrayDropOp,
        "array/range" => K::ArrayRangeOp,

        // Phase 3.2: String Advanced
        "str/slice" => K::StrSliceOp,
        "str/padStart" => K::StrPadStartOp,
        "str/padEnd" => K::StrPadEndOp,
        "str/capitalize" => K::StrCapitalizeOp,
        "str/truncate" => K::StrTruncateOp,

        // Phase 3.3: Object Advanced
        "object/fromEntries" => K::ObjectFromEntriesOp,
        "object/deepMerge" => K::ObjectDeepMergeOp,
        "object/pick" => K::ObjectPickOp,
        "object/omit" => K::ObjectOmitOp,
        "object/mapValues" => K::ObjectMapValuesOp,
        "object/filter" => K::ObjectFilterOp,

        // Phase 3.4: Math Advanced
        "math/sign" => K::MathSignOp,
        "math/lerp" => K::MathLerpOp,
        "math/map" => K::MathMapOp,

        // Phase 3.5: Time Advanced
        "time/year" => K::TimeYearOp,
        "time/month" => K::TimeMonthOp,
        "time/day" => K::TimeDayOp,
        "time/weekday" => K::TimeWeekdayOp,
        "time/hour" => K::TimeHourOp,
        "time/minute" => K::TimeMinuteOp,
        "time/second" => K::TimeSecondOp,
        "time/isBetween" => K::TimeIsBetweenOp,
        "time/isSame" => K::TimeIsSameOp,

        // Phase 3.6: Validation Advanced
        "validate/uuid" => K::ValidateUuidOp,
        "validate/date" => K::ValidateDateOp,
        "validate/length" => K::ValidateLengthOp,
        "validate/range" => K::ValidateRangeOp,
        "validate/oneOf" => K::ValidateOneOfOp,
        "validate/equals" => K::ValidateEqualsOp,

        // Phase 4.1: Array Extras
        "array/shuffle" => K::ArrayShuffleOp,
        "array/reject" => K::ArrayRejectOp,
        "array/partition" => K::ArrayPartitionOp,
        "array/takeLast" => K::ArrayTakeLastOp,
        "array/dropLast" => K::ArrayDropLastOp,

        // Phase 4.2: String Extras
        "str/trimStart" => K::StrTrimStartOp,
        "str/trimEnd" => K::StrTrimEndOp,
        "str/repeat" => K::StrRepeatOp,
        "str/reverse" => K::StrReverseOp,
        "str/encode-uri-component" => K::StrEncodeUriComponentOp,
        "str/decode-uri-component" => K::StrDecodeUriComponentOp,
        "str/uri-component-valid" => K::StrUriComponentValidOp,
        "str/to-number" => K::StrToNumberOp,
        "str/to-int" => K::StrToIntOp,
        "str/collate" => K::StrCollateOp,
        "str/collate-key" => K::StrCollateKeyOp,
        "bit/xor" => K::BitXorOp,
        "bit/and" => K::BitAndOp,
        "bit/or" => K::BitOrOp,
        "bit/not" => K::BitNotOp,
        "bit/shl" => K::BitShlOp,
        "bit/shr" => K::BitShrOp,
        "bit/rotl" => K::BitRotlOp,
        "bit/rotr" => K::BitRotrOp,
        "bytes/from-string" => K::BytesFromStringOp,
        "bytes/to-string" => K::BytesToStringOp,
        "bytes/from-base64" => K::BytesFromBase64Op,
        "bytes/to-base64" => K::BytesToBase64Op,
        "bytes/length" => K::BytesLengthOp,
        "bytes/slice" => K::BytesSliceOp,
        "bytes/concat" => K::BytesConcatOp,
        "bytes/equals" => K::BytesEqualsOp,
        "bytes/index-of" => K::BytesIndexOfOp,
        "bytes/at" => K::BytesAtOp,
        "bytes/read-uint" => K::BytesReadUintOp,
        "bytes/write-uint" => K::BytesWriteUintOp,
        "base64/encode" => K::Base64EncodeOp,
        "base64/decode" => K::Base64DecodeOp,
        "base64/valid" => K::Base64ValidOp,
        "hash/sha256" => K::HashSha256Op,
        "hash/md5" => K::HashMd5Op,
        "hash/scrypt" => K::HashScryptOp,
        "hash/pbkdf2" => K::HashPbkdf2Op,
        "hmac/sha256" => K::HmacSha256Op,
        "gzip/compress" => K::GzipCompressOp,
        "gzip/decompress" => K::GzipDecompressOp,
        "brotli/compress" => K::BrotliCompressOp,
        "brotli/decompress" => K::BrotliDecompressOp,
        "str/titleCase" => K::StrTitleCaseOp,
        "str/camelCase" => K::StrCamelCaseOp,
        "str/kebabCase" => K::StrKebabCaseOp,
        "str/snakeCase" => K::StrSnakeCaseOp,

        // Phase 4.3: Object Extras
        "object/mapKeys" => K::ObjectMapKeysOp,
        "object/equals" => K::ObjectEqualsOp,
        "object/clone" => K::ObjectCloneOp,
        "object/deepClone" => K::ObjectDeepCloneOp,

        // Phase 4.4: Math Extras
        "math/random" => K::MathRandomOp,
        "math/randomInt" => K::MathRandomIntOp,

        // Phase 4.5: Time Extras
        "time/isPast" => K::TimeIsPastOp,
        "time/isFuture" => K::TimeIsFutureOp,
        "time/isToday" => K::TimeIsTodayOp,
        "time/duration" => K::TimeDurationOp,

        // Phase 4.6: Validation Extras
        "validate/phone" => K::ValidatePhoneOp,
        "validate/creditCard" => K::ValidateCreditCardOp,
        "validate/noneOf" => K::ValidateNoneOfOp,
        "validate/check" => K::ValidateCheckOp,

        // Phase 5: Parity Completion
        "array/max" => K::ArrayMaxOp,
        "array/min" => K::ArrayMinOp,
        "math/mod" => K::MathModOp,
        "validate/max" => K::ValidateMaxOp,
        "validate/min" => K::ValidateMinOp,
        "format/number" => K::FormatNumberOp,
        "format/percent" => K::FormatPercentOp,
        "format/currency" => K::FormatCurrencyOp,
        "i18n/t" => K::I18nTOp,
        "format/bytes" => K::FormatBytesOp,
        "format/ordinal" => K::FormatOrdinalOp,
        "format/phone" => K::FormatPhoneOp,
        "format/plural" => K::FormatPluralOp,
        "format/list" => K::FormatListOp,

        // ====================================================================
        // Composition (compile-time, parity placeholder)
        // Real implementation lives in orbital-compiler Compose phase (0.3).
        // ====================================================================
        "behavior/apply" => K::BehaviorApplyOp,
        "behavior/ref" => K::BehaviorRefOp,
        "behavior/compose" => K::BehaviorCompositionStubOp("behavior/compose"),
        "behavior/wire" => K::BehaviorCompositionStubOp("behavior/wire"),
        "behavior/detect-layout" => K::BehaviorCompositionStubOp("behavior/detect-layout"),
        "behavior/pipe" => K::BehaviorCompositionStubOp("behavior/pipe"),

        // prob/*: seeded sampling, rejection-sampling inference, sample statistics
        "prob/beta" => K::ProbBetaOp,
        "prob/categorical" => K::ProbCategoricalOp,
        "prob/condition" => K::ProbConditionOp,
        "prob/credible-interval" => K::ProbCredibleIntervalOp,
        "prob/expected-value" => K::ProbExpectedValueOp,
        "prob/flip" => K::ProbFlipOp,
        "prob/gaussian" => K::ProbGaussianOp,
        "prob/histogram" => K::ProbHistogramOp,
        "prob/infer" => K::ProbInferOp,
        "prob/percentile" => K::ProbPercentileOp,
        "prob/poisson" => K::ProbPoissonOp,
        "prob/posterior" => K::ProbPosteriorOp,
        "prob/sample" => K::ProbSampleOp,
        "prob/seed" => K::ProbSeedOp,
        "prob/uniform" => K::ProbUniformOp,
        "prob/variance" => K::ProbVarianceOp,

        // Pure operators added for canonical-registry parity.
        "array/count" => K::CountOp,
        "array/removeItem" => K::ArrayRemoveItemOp,
        "time/startOf" => K::TimeStartOfOp,
        "time/endOf" => K::TimeEndOfOp,
        "time/relative" => K::TimeRelativeOp,
        "format/creditCard" => K::FormatCreditCardOp,
        "list" => K::ListOp,

        // Agent-runtime operators — stubbed (require the agent runtime).

        // ====================================================================
        // G6 Game operators (Wave 1a)
        // ====================================================================

        // math/* (game trig & angle helpers)
        "math/sin" => K::MathSinOp,
        "math/cos" => K::MathCosOp,
        "math/tan" => K::MathTanOp,
        "math/atan2" => K::MathAtan2Op,
        "math/asin" => K::MathAsinOp,
        "math/acos" => K::MathAcosOp,
        "math/atan" => K::MathAtanOp,
        "math/hypot" => K::MathHypotOp,
        "math/deg-rad" => K::MathDegRadOp,
        "math/rad-deg" => K::MathRadDegOp,
        "math/wrap" => K::MathWrapOp,
        "math/approach" => K::MathApproachOp,

        // vec/*
        "vec/add" => K::VecAddOp,
        "vec/sub" => K::VecSubOp,
        "vec/scale" => K::VecScaleOp,
        "vec/dot" => K::VecDotOp,
        "vec/cross" => K::VecCrossOp,
        "vec/length" => K::VecLengthOp,
        "vec/length-sq" => K::VecLengthSqOp,
        "vec/normalize" => K::VecNormalizeOp,
        "vec/distance" => K::VecDistanceOp,
        "vec/distance-sq" => K::VecDistanceSqOp,
        "vec/lerp" => K::VecLerpOp,
        "vec/angle" => K::VecAngleOp,
        "vec/rotate" => K::VecRotateOp,
        "vec/clamp-length" => K::VecClampLengthOp,

        // geo/*
        "geo/aabb-overlap" => K::GeoAabbOverlapOp,
        "geo/circle-overlap" => K::GeoCircleOverlapOp,
        "geo/rect-circle-overlap" => K::GeoRectCircleOverlapOp,
        "geo/point-in-rect" => K::GeoPointInRectOp,
        "geo/point-in-circle" => K::GeoPointInCircleOp,
        "geo/reflect" => K::GeoReflectOp,
        "geo/segment-intersect" => K::GeoSegmentIntersectOp,

        // grid/*
        "grid/to-world" => K::GridToWorldOp,
        "grid/from-world" => K::GridFromWorldOp,
        "grid/iso-to-screen" => K::GridIsoToScreenOp,
        "grid/screen-to-iso" => K::GridScreenToIsoOp,
        "grid/distance" => K::GridDistanceOp,
        "grid/manhattan-distance" => K::GridManhattanDistanceOp,
        "grid/neighbors" => K::GridNeighborsOp,
        "grid/cells-in-radius" => K::GridCellsInRadiusOp,
        "grid/line" => K::GridLineOp,
        "grid/in-bounds" => K::GridInBoundsOp,

        // anim/*
        "anim/frame-at" => K::AnimFrameAtOp,
        "anim/sheet-rect" => K::AnimSheetRectOp,
        "anim/direction-from-delta" => K::AnimDirectionFromDeltaOp,

        // ease/*
        "ease/apply" => K::EaseApplyOp,
        "ease/smoothstep" => K::EaseSmoothstepOp,

        // ====================================================================
        // G6 Game operators (Wave 1b)
        // ====================================================================

        // noise/*
        "noise/perlin" => K::NoisePerlinOp,
        "noise/simplex" => K::NoiseSimplexOp,
        "noise/fbm" => K::NoiseFbmOp,

        // path/*
        "path/astar" => K::PathAstarOp,
        "path/reachable" => K::PathReachableOp,

        // os/path-* (string math, no OS access)
        "os/path-join" => K::OsPathJoinOp,
        "os/path-normalize" => K::OsPathNormalizeOp,
        "os/path-base" => K::OsPathBaseOp,
        "os/path-dir" => K::OsPathDirOp,
        "os/path-ext" => K::OsPathExtOp,
        "os/path-relative" => K::OsPathRelativeOp,
        "os/path-is-absolute" => K::OsPathIsAbsoluteOp,

}

/// Core expression evaluator - used by operators. The kernel trusts validated types: an operator raises where it
/// reads an argument of the wrong kind, and no per-argument signature check runs here.
pub fn eval_expr(expr: &SExpression, ctx: &BindingContext) -> EvalResult {
    if super::trace::is_active() {
        return super::trace::step(expr, ctx, eval_node);
    }
    eval_node(expr, ctx)
}

/// `f` over an operand's value: a plain binding (`@entity.x`, a `let` name) is read where it is bound, so a reader
/// that needs part of it copies only that part; any other operand, or a binding [`BindingContext::resolve_borrowed`]
/// cannot read in place, is evaluated as [`eval_expr`] would.
pub(crate) fn with_operand<R>(
    expr: &SExpression,
    ctx: &BindingContext,
    f: impl FnOnce(&Value) -> Result<R, OrbitalError>,
) -> Result<R, OrbitalError> {
    if let SExpression::String(s) = expr {
        if let Some(value) = s
            .strip_prefix('@')
            .and_then(|path| ctx.resolve_borrowed(path))
        {
            return f(value);
        }
    }
    let value = eval_expr(expr, ctx)?;
    f(&value)
}

fn eval_node(expr: &SExpression, ctx: &BindingContext) -> EvalResult {
    match expr {
        // Primitives
        SExpression::Null => Ok(Value::Null),
        SExpression::Bool(b) => Ok(Value::Bool(*b)),
        SExpression::Number(n) => Ok(Value::Number(*n)),

        // String - may be a binding reference
        SExpression::String(s) => {
            if s.starts_with('@') {
                // Return Null for missing bindings (similar to JavaScript undefined)
                // This allows guards to check for existence with != null
                match ctx.resolve_binding(s) {
                    Some(value) => Ok(value),
                    // A client-only frame reference stays for the client to embed, as the value resolver keeps it.
                    None if s.starts_with("@trait.") => Ok(Value::string(s.clone())),
                    None => Ok(Value::Null),
                }
            } else {
                Ok(Value::string(s.clone()))
            }
        }

        // Object - evaluate all values
        SExpression::Object(obj) => {
            let mut result = ValueMap::default();
            for (k, v) in obj {
                result.insert(k.clone(), eval_expr(v, ctx)?);
            }
            Ok(Value::object(result))
        }

        // Array - either a function call or a literal array
        SExpression::Array(arr) => {
            if arr.is_empty() {
                return Ok(Value::array(vec![]));
            }

            // Check if this is a function call (first element is a string operator)
            if let SExpression::String(op) = &arr[0] {
                // Bare "concat" is ambiguous between string and array concat
                // and has no dispatch entry (R-EVAL-CONCAT-PATH-DIVERGENCE) —
                // reject it explicitly rather than silently falling through
                // to the literal-array path below.
                if op == "concat" {
                    return Err(EvalError::UnknownOperator(
                        "concat (ambiguous — use 'str/concat' or 'array/concat')".to_string(),
                    )
                    .into());
                }
                if let Some(operator) = get_operator(op) {
                    if let Some(expected) = arg_types::declared_signature(&operator).and_then(|signature| signature.arity_breach(arr.len() - 1)) {
                        return Err(EvalError::ArityMismatch { operator: op.clone().into_inner(), expected, actual: arr.len() - 1 }.into());
                    }
                    return operator.evaluate(&arr[1..], ctx).inspect_err(|error| {
                        tracing::debug!(target: orbital_core_base::logging::ns::EVAL_RAISE, operator = %op, %error, call = %serde_json::to_string(expr).unwrap_or_default(), "operator raised");
                    });
                }
                // A namespaced head is an operator by the language's naming rule: one the installed table does not
                // know is refused, never read as data (owner ruling 2026-10-24; data lists like it are quoted).
                if op.contains('/')
                    && orbital_core_base::language_data::operator_data().is_ok()
                    && orbital_core_base::language::registry::get_operator(op).is_none()
                    && !orbital_core_base::effect_types::is_known_effect_name(op)
                {
                    return Err(EvalError::UnknownOperator(op.clone().into_inner()).into());
                }
            }

            // Literal array - evaluate all elements
            let values: Result<Vec<Value>, _> = arr.iter().map(|e| eval_expr(e, ctx)).collect();
            Ok(Value::array(values?))
        }
    }
}
