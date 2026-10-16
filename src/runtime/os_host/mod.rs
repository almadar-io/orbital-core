//! The native answerer of [`IoRequest::Os`]: one place every native host reaches through `answer_http`.
//! Wasm hosts answer `Os` themselves, as they do `Random`.

use crate::runtime::os::{OsError, OsErrorKind};
use crate::runtime::resumable::{IoAnswer, IoRequest};

pub fn answer_os(request: &IoRequest) -> IoAnswer {
    match request {
        IoRequest::Os { op, .. } => IoAnswer::Os(Err(OsError::new(
            OsErrorKind::Unsupported,
            format!(
                "{}: this host does not implement the operation yet",
                op.name()
            ),
        ))),
        _ => IoAnswer::Service(Err("not an os request".to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::os::OsOp;

    #[test]
    fn every_os_ask_is_unsupported_until_the_host_fills_it() {
        for op in OsOp::ALL {
            let ask = IoRequest::Os {
                op: *op,
                args: Vec::new(),
                key: None,
                site: None,
            };
            match answer_os(&ask) {
                IoAnswer::Os(Err(error)) => assert_eq!(error.kind, OsErrorKind::Unsupported),
                other => panic!("{} answered {other:?}", op.name()),
            }
        }
    }

    #[test]
    fn a_request_that_is_not_an_os_ask_is_not_answered_as_one() {
        let ask = IoRequest::Random { bytes: 1 };
        assert!(!matches!(answer_os(&ask), IoAnswer::Os(_)));
    }
}
