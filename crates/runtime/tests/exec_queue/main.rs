//! EXEC-QUEUE family: who hears an emit, the cascade worklist and keyed completions.

#[path = "../support/mod.rs"]
mod support;

mod conformance;
mod call_service_site;
mod cascade_visit_key;
mod cross_trait_dispatch;
mod delivery_payload;
mod dispatch_roots;
mod emit_marked_dispatched;
mod listen_source_identity;
mod listener_delivery_does_not_refan;
mod own_emit_fans_out_once;
mod per_listen_instances;
mod projected_payload;
mod qualified_listen_without_source;
mod relay_payload_fields_do_not_key_frames;
mod same_trait_cascade;
mod untargeted_discovery;
