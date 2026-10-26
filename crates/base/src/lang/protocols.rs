//! The endpoint protocol table (EXEC-ENDPOINT-1): language data read like the
//! operator table. Each row names a protocol, the record type of its address,
//! whether a viewer exists, the grant it needs and the side that serves it.
//! A protocol's behavior is std `.lolo`; a row only declares it. The table is
//! `@almadar/core`'s `orbLanguage.protocols`; a compiled client program carries
//! it in `_metadata.protocols` for a kernel with no installed core.

#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use serde::{Deserialize, Serialize};

use crate::language_data::{language_data, LanguageDataError};
use crate::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProtocolSide {
    Client,
    Server,
    Process,
    Start,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GrantKind {
    NetListen,
    Stdio,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddressField {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolSpec {
    pub name: String,
    pub address: Option<Vec<AddressField>>,
    pub viewer: bool,
    pub grant: Option<GrantKind>,
    pub side: ProtocolSide,
}

/// A browser route: a page written with a `"/…"` path literal and no protocol word.
pub const ROUTE: &str = "route";

/// The rows of `text` (`orbLanguage.protocols`): a non-empty array of unique names that includes the route row.
pub fn parse(text: &str) -> Result<Vec<ProtocolSpec>, String> {
    let rows: Vec<ProtocolSpec> =
        serde_json::from_str(text).map_err(|e| format!("invalid protocols: {e}"))?;
    if rows.is_empty() {
        return Err("the protocol table has no rows".to_string());
    }
    for (i, row) in rows.iter().enumerate() {
        if rows[..i].iter().any(|earlier| earlier.name == row.name) {
            return Err(format!("protocol `{}` is declared twice", row.name));
        }
    }
    if !rows.iter().any(|row| row.name == ROUTE) {
        return Err(format!("the protocol table has no `{ROUTE}` row"));
    }
    Ok(rows)
}

static TABLE: OnceLock<Vec<ProtocolSpec>> = OnceLock::new();
static PROGRAM_TABLE: OnceLock<Vec<ProtocolSpec>> = OnceLock::new();

/// The installed language's protocol table, else the one a registered program carried; a typed failure when neither
/// exists, never an empty table.
pub fn table() -> Result<&'static [ProtocolSpec], LanguageDataError> {
    if let Some(rows) = TABLE.get() {
        return Ok(rows.as_slice());
    }
    match language_data() {
        Ok(data) => {
            let rows = parse(&data.protocols).map_err(|reason| LanguageDataError::Invalid {
                path: data.root.clone(),
                reason,
            })?;
            Ok(TABLE.get_or_init(|| rows).as_slice())
        }
        Err(LanguageDataError::NotInstalled { .. }) if PROGRAM_TABLE.get().is_some() => {
            Ok(PROGRAM_TABLE.get().map(Vec::as_slice).unwrap_or_default())
        }
        Err(e) => Err(e),
    }
}

/// [`table`] for callers that cannot return a `Result`; every entry point that can calls [`table`] or the language
/// preflight first, so reaching the panic means one skipped it.
pub fn table_after_preflight() -> &'static [ProtocolSpec] {
    match table() {
        Ok(rows) => rows,
        Err(e) => panic!(
            "{}: {e} (the entry point must call preflight first)",
            e.code()
        ),
    }
}

/// Installs the table a compiled program carries; refuses one that differs from one already installed.
pub fn install_program_protocols(rows: Vec<ProtocolSpec>) -> Result<(), String> {
    match PROGRAM_TABLE.get() {
        Some(installed) if *installed != rows => Err(format!(
            "protocols: the program carries {rows:?}, but this kernel already runs a program with {installed:?}"
        )),
        Some(_) => Ok(()),
        None => {
            let _ = PROGRAM_TABLE.set(rows);
            Ok(())
        }
    }
}

pub fn protocol(name: &str) -> Result<Option<&'static ProtocolSpec>, LanguageDataError> {
    Ok(table()?.iter().find(|p| p.name == name))
}

/// [`protocol`] for callers that cannot return a `Result`; see [`table_after_preflight`].
pub fn protocol_after_preflight(name: &str) -> Option<&'static ProtocolSpec> {
    table_after_preflight().iter().find(|p| p.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(
        name: &str,
        address: serde_json::Value,
        viewer: bool,
        grant: serde_json::Value,
        side: &str,
    ) -> serde_json::Value {
        json!({ "name": name, "address": address, "viewer": viewer, "grant": grant, "side": side })
    }

    fn rows_json() -> serde_json::Value {
        json!([
            row("route", json!(null), true, json!(null), "client"),
            row(
                "http",
                json!([
                    { "name": "host", "type": "string", "required": true },
                    { "name": "port", "type": "number", "required": true },
                    { "name": "path", "type": "string", "required": false }
                ]),
                false,
                json!("net-listen"),
                "server"
            ),
            row("stdio", json!(null), false, json!("stdio"), "process"),
            row("boot", json!(null), false, json!(null), "start"),
        ])
    }

    fn rows() -> Vec<ProtocolSpec> {
        parse(&rows_json().to_string()).expect("the four rows parse")
    }

    fn find<'a>(rows: &'a [ProtocolSpec], name: &str) -> Option<&'a ProtocolSpec> {
        rows.iter().find(|p| p.name == name)
    }

    #[test]
    fn the_installed_table_is_the_four_rows_the_constant_held() {
        assert_eq!(table().expect("core is installed"), rows().as_slice());
    }

    #[test]
    fn the_table_has_the_four_rows_in_order() {
        let rows = rows();
        let names: Vec<&str> = rows.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["route", "http", "stdio", "boot"]);
    }

    // EXEC-ENDPOINT-1
    #[test]
    fn http_takes_a_host_port_path_address_with_host_and_port_required() {
        let rows = rows();
        let http = find(&rows, "http").expect("http row");
        let address = http.address.as_ref().expect("http address");
        let shape: Vec<(&str, &str, bool)> = address
            .iter()
            .map(|f| (f.name.as_str(), f.ty.as_str(), f.required))
            .collect();
        assert_eq!(
            shape,
            [
                ("host", "string", true),
                ("port", "number", true),
                ("path", "string", false)
            ]
        );
        assert_eq!(
            (http.viewer, http.grant, http.side),
            (false, Some(GrantKind::NetListen), ProtocolSide::Server)
        );
    }

    #[test]
    fn route_is_the_only_viewer_and_the_only_client_side() {
        let rows = rows();
        let viewers: Vec<&str> = rows
            .iter()
            .filter(|p| p.viewer)
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(viewers, ["route"]);
        let clients: Vec<&str> = rows
            .iter()
            .filter(|p| p.side == ProtocolSide::Client)
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(clients, ["route"]);
    }

    #[test]
    fn stdio_and_boot_take_no_address_and_stdio_needs_its_grant() {
        let rows = rows();
        for name in ["route", "stdio", "boot"] {
            assert!(find(&rows, name).unwrap().address.is_none(), "{name}");
        }
        assert_eq!(find(&rows, "stdio").unwrap().grant, Some(GrantKind::Stdio));
        assert_eq!(find(&rows, "stdio").unwrap().side, ProtocolSide::Process);
        assert_eq!(find(&rows, "boot").unwrap().grant, None);
        assert_eq!(find(&rows, "boot").unwrap().side, ProtocolSide::Start);
    }

    #[test]
    fn an_unknown_protocol_is_none() {
        assert!(protocol("grpc").expect("core is installed").is_none());
        assert!(protocol("").expect("core is installed").is_none());
        assert_eq!(
            protocol("http")
                .expect("core is installed")
                .map(|p| p.name.as_str()),
            Some("http")
        );
    }

    #[test]
    fn the_table_serializes_back_to_the_shape_it_was_read_from() {
        let json = serde_json::to_value(rows()).unwrap();
        assert_eq!(json, rows_json());
        assert_eq!(json[1]["grant"], "net-listen");
        assert_eq!(
            json[1]["address"][1],
            json!({ "name": "port", "type": "number", "required": true })
        );
        assert!(json[0]["address"].is_null());
    }

    #[test]
    fn edge_a_malformed_table_is_refused_naming_why() {
        assert!(parse("[]").unwrap_err().contains("no rows"));
        assert!(parse("{}").unwrap_err().contains("invalid protocols"));
        let mut twice = rows_json();
        twice
            .as_array_mut()
            .unwrap()
            .push(row("boot", json!(null), false, json!(null), "start"));
        assert!(parse(&twice.to_string())
            .unwrap_err()
            .contains("`boot` is declared twice"));
        let mut no_route = rows_json();
        no_route.as_array_mut().unwrap().remove(0);
        assert!(parse(&no_route.to_string())
            .unwrap_err()
            .contains("no `route` row"));
        let mut bad_side = rows_json();
        bad_side[3]["side"] = json!("moon");
        assert!(parse(&bad_side.to_string())
            .unwrap_err()
            .contains("invalid protocols"));
        let mut bad_grant = rows_json();
        bad_grant[1]["grant"] = json!("net-everything");
        assert!(parse(&bad_grant.to_string())
            .unwrap_err()
            .contains("invalid protocols"));
        let mut extra = rows_json();
        extra[3]["sied"] = json!("start");
        assert!(parse(&extra.to_string())
            .unwrap_err()
            .contains("invalid protocols"));
    }

    #[test]
    fn a_program_s_table_installs_once_and_a_different_one_is_refused() {
        let mut other = rows();
        other.pop();
        assert_eq!(install_program_protocols(rows()), Ok(()));
        assert_eq!(
            install_program_protocols(rows()),
            Ok(()),
            "a second program with the same table"
        );
        assert!(install_program_protocols(other)
            .unwrap_err()
            .contains("already runs a program"));
    }
}
