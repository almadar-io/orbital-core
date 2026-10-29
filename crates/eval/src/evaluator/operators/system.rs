//! Time, validation, format, lambda, and composition operators (the
//! system-facing remainder of the operator set).
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::util::deep_equals;
use super::*;

use super::string;

mod composition;
mod formatting;
mod lambda;
mod time;
mod validation;
use composition::*;
pub use composition::{
    BehaviorCompositionStubOp, FormatCreditCardOp, TimeEndOfOp, TimeRelativeOp, TimeStartOfOp,
};
pub use formatting::{FormatOrdinalOp, FormatPluralOp};
pub use lambda::{BehaviorApplyOp, BehaviorRefOp, FnOp, QuasiquoteOp, QuoteOp, UnquoteOp};
use time::*;
pub use time::{
    TimeAddOp, TimeDayOp, TimeDiffOp, TimeDurationOp, TimeFormatOp, TimeHourOp, TimeIsAfterOp,
    TimeIsBeforeOp, TimeIsBetweenOp, TimeIsFutureOp, TimeIsPastOp, TimeIsSameOp, TimeIsTodayOp,
    TimeMinuteOp, TimeMonthOp, TimeNowOp, TimeParseOp, TimeSecondOp, TimeSubtractOp, TimeTodayOp,
    TimeUnit, TimeValidOp, TimeWeekdayOp, TimeYearOp, TIME_UNIT_VOCABULARY,
};
pub(crate) use time::instant_value;
pub use time::{parse_iso, Shape as IsoShape};
pub use validation::{
    matches_semantic_pattern, semantic_string_value_valid, ValidateArrayOp, ValidateBooleanOp,
    ValidateCheckOp, ValidateCreditCardOp, ValidateDateOp, ValidateEmailOp, ValidateEqualsOp,
    ValidateLengthOp, ValidateMaxLengthOp, ValidateMaxOp, ValidateMinLengthOp, ValidateMinOp,
    ValidateNoneOfOp, ValidateNumberOp, ValidateObjectOp, ValidateOneOfOp, ValidatePatternOp,
    ValidatePhoneOp, ValidateRangeOp, ValidateRequiredOp, ValidateStringOp, ValidateUrlOp,
    ValidateUuidOp,
};
