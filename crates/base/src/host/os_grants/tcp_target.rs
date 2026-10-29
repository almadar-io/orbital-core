//! A TCP protocol target (`redis://host[:port]`, `postgres://host[:port]`, ...) against the program's `egress`
//! list: the origin check the `os/tcp-connect` grant sits beside.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::http_wire::{parse_egress_entry, split_tcp_url, HttpRefusal, OriginParts};

/// A TCP protocol driver's connect target against `egress`: exact scheme, host and port, the default port of the
/// scheme when none is written. Refuses as `http` does, with [`HttpRefusal::EgressDenied`]; an `http(s)` entry never
/// authorizes a TCP target.
pub fn tcp_target_allowed(origin: &str, egress: &[String]) -> Result<OriginParts, HttpRefusal> {
    let denied = || HttpRefusal::EgressDenied(origin.to_string());
    let (target, rest) = split_tcp_url(origin).ok_or_else(denied)?;
    if !rest.is_empty() {
        return Err(denied());
    }
    if egress
        .iter()
        .filter_map(|entry| parse_egress_entry(entry))
        .any(|allowed| allowed == target)
    {
        Ok(target)
    } else {
        Err(denied())
    }
}
