use crate::bus_event::BusEventSource;
use crate::schema::types::ListenSource;


/// The source identity rule shared by runtime delivery and static route validation.
pub fn matches_listen_source(
    declared: Option<&ListenSource>,
    listener_orbital: &str,
    source: &BusEventSource,
) -> bool {
    match declared {
        None | Some(ListenSource::Any) => true,
        Some(ListenSource::Trait {
            trait_name,
            trait_id,
        }) => match trait_id {
            Some(id) => source.trait_id.as_deref() == Some(id.as_str()),
            None => {
                source.orbital.as_deref() == Some(listener_orbital)
                    && source.r#trait.as_deref() == Some(trait_name.as_str())
            }
        },
        Some(ListenSource::Orbital {
            orbital,
            orbital_id,
            trait_name,
            trait_id,
        }) => match (orbital_id, trait_id) {
            (Some(orbital_id), Some(trait_id)) => {
                source.orbital_id.as_deref() == Some(orbital_id.as_str())
                    && source.trait_id.as_deref() == Some(trait_id.as_str())
            }
            _ => {
                source.orbital.as_deref() == Some(orbital.as_str())
                    && source.r#trait.as_deref() == Some(trait_name.as_str())
            }
        },
    }
}