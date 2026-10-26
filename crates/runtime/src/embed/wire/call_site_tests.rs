use super::*;

fn site(entity_id: &str) -> CallSite {
    CallSite {
        orbital: None,
        trait_name: "Sup".to_string(),
        follow_on: false,
        entity_id: entity_id.to_string(),
        emit: CallRoutes::default(),
    }
}

#[test]
fn a_call_site_keeps_its_frame_across_the_wire_and_the_singleton_stays_off_it() {
    let row = CallSiteWire::from(&site("row-7"));
    assert_eq!(row.entity_id.as_deref(), Some("row-7"));
    assert_eq!(CallSite::from(row).entity_id, "row-7");
    let single = CallSiteWire::from(&site(crate::process_result::RUNTIME_SINGLETON));
    assert_eq!(single.entity_id, None, "the singleton is the absence of a frame on the wire");
    assert_eq!(CallSite::from(single).entity_id, crate::process_result::RUNTIME_SINGLETON);
    let old: CallSiteWire = serde_json::from_str(r#"{"trait":"Sup","followOn":false,"emit":{}}"#).expect("an older site parses");
    assert_eq!(CallSite::from(old).entity_id, crate::process_result::RUNTIME_SINGLETON, "control: a site with no frame addresses the singleton");
}
