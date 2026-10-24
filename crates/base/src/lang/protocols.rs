//! The endpoint protocol table (EXEC-ENDPOINT-1): language data read like the
//! operator table. Each row names a protocol, the record type of its address,
//! whether a viewer exists, the grant it needs and the side that serves it.
//! A protocol's behavior is std `.lolo`; a row only declares it.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProtocolSide {
    Client,
    Server,
    Process,
    Start,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum GrantKind {
    NetListen,
    Stdio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AddressField {
    pub name: &'static str,
    #[serde(rename = "type")]
    pub ty: &'static str,
    pub required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProtocolSpec {
    pub name: &'static str,
    pub address: Option<&'static [AddressField]>,
    pub viewer: bool,
    pub grant: Option<GrantKind>,
    pub side: ProtocolSide,
}

const HTTP_ADDRESS: &[AddressField] = &[
    AddressField { name: "host", ty: "string", required: true },
    AddressField { name: "port", ty: "number", required: true },
    AddressField { name: "path", ty: "string", required: false },
];

pub const PROTOCOLS: &[ProtocolSpec] = &[
    ProtocolSpec { name: "route", address: None, viewer: true, grant: None, side: ProtocolSide::Client },
    ProtocolSpec { name: "http", address: Some(HTTP_ADDRESS), viewer: false, grant: Some(GrantKind::NetListen), side: ProtocolSide::Server },
    ProtocolSpec { name: "stdio", address: None, viewer: false, grant: Some(GrantKind::Stdio), side: ProtocolSide::Process },
    ProtocolSpec { name: "boot", address: None, viewer: false, grant: None, side: ProtocolSide::Start },
];

pub fn protocol(name: &str) -> Option<&'static ProtocolSpec> {
    PROTOCOLS.iter().find(|p| p.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_has_the_four_rows_in_order() {
        let names: Vec<&str> = PROTOCOLS.iter().map(|p| p.name).collect();
        assert_eq!(names, ["route", "http", "stdio", "boot"]);
    }

    #[test]
    fn http_takes_a_host_port_path_address_with_host_and_port_required() {
        let http = protocol("http").expect("http row");
        let address = http.address.expect("http address");
        let shape: Vec<(&str, &str, bool)> = address.iter().map(|f| (f.name, f.ty, f.required)).collect();
        assert_eq!(shape, [("host", "string", true), ("port", "number", true), ("path", "string", false)]);
        assert_eq!((http.viewer, http.grant, http.side), (false, Some(GrantKind::NetListen), ProtocolSide::Server));
    }

    #[test]
    fn route_is_the_only_viewer_and_the_only_client_side() {
        let viewers: Vec<&str> = PROTOCOLS.iter().filter(|p| p.viewer).map(|p| p.name).collect();
        assert_eq!(viewers, ["route"]);
        let clients: Vec<&str> = PROTOCOLS.iter().filter(|p| p.side == ProtocolSide::Client).map(|p| p.name).collect();
        assert_eq!(clients, ["route"]);
    }

    #[test]
    fn stdio_and_boot_take_no_address_and_stdio_needs_its_grant() {
        for name in ["route", "stdio", "boot"] {
            assert!(protocol(name).unwrap().address.is_none(), "{name}");
        }
        assert_eq!(protocol("stdio").unwrap().grant, Some(GrantKind::Stdio));
        assert_eq!(protocol("stdio").unwrap().side, ProtocolSide::Process);
        assert_eq!(protocol("boot").unwrap().grant, None);
        assert_eq!(protocol("boot").unwrap().side, ProtocolSide::Start);
    }

    #[test]
    fn an_unknown_protocol_is_none() {
        assert!(protocol("grpc").is_none());
        assert!(protocol("").is_none());
    }

    #[test]
    fn the_table_serializes_for_the_language_reference() {
        let json = serde_json::to_value(PROTOCOLS).unwrap();
        assert_eq!(json[1]["name"], "http");
        assert_eq!(json[1]["grant"], "net-listen");
        assert_eq!(json[1]["side"], "server");
        assert_eq!(json[1]["address"][1], serde_json::json!({ "name": "port", "type": "number", "required": true }));
        assert!(json[0]["address"].is_null());
    }
}
