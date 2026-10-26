//! Time, validation, format, lambda, and composition operators (the
//! system-facing remainder of the operator set).
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::util::deep_equals;
use super::*;

use super::string;

mod time;
mod validation;
mod formatting;
mod lambda;
mod composition;
use time::*;
use composition::*;
pub use composition::{
    AgentStubOp, BehaviorCompositionStubOp, FormatCreditCardOp, TimeEndOfOp, TimeRelativeOp,
    TimeStartOfOp,
};
pub use formatting::{FormatOrdinalOp, FormatPluralOp};
pub use lambda::{BehaviorApplyOp, BehaviorRefOp, FnOp, QuasiquoteOp, QuoteOp, UnquoteOp};
pub use time::{
    TimeAddOp, TimeDayOp, TimeDiffOp, TimeDurationOp, TimeFormatOp, TimeHourOp, TimeIsAfterOp,
    TimeIsBeforeOp, TimeIsBetweenOp, TimeIsFutureOp, TimeIsPastOp, TimeIsSameOp, TimeIsTodayOp,
    TimeMinuteOp, TimeMonthOp, TimeNowOp, TimeParseOp, TimeSecondOp, TimeSubtractOp, TimeTodayOp,
    TimeUnit, TimeValidOp, TimeWeekdayOp, TimeYearOp, TIME_UNIT_VOCABULARY,
};
pub use validation::{
    matches_semantic_pattern, semantic_string_value_valid, ValidateArrayOp, ValidateBooleanOp,
    ValidateCheckOp, ValidateCreditCardOp, ValidateDateOp, ValidateEmailOp, ValidateEqualsOp,
    ValidateLengthOp, ValidateMaxLengthOp, ValidateMaxOp, ValidateMinLengthOp, ValidateMinOp,
    ValidateNoneOfOp, ValidateNumberOp, ValidateObjectOp, ValidateOneOfOp, ValidatePatternOp,
    ValidatePhoneOp, ValidateRangeOp, ValidateRequiredOp, ValidateStringOp, ValidateUrlOp,
    ValidateUuidOp,
};
