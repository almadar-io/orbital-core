//! Static check of WGSL source with naga: the parse and validation error text, and the bindings a valid module declares.

use naga::valid::{Capabilities, ValidationFlags, Validator};
use naga::{AddressSpace, StorageAccess, TypeInner};

#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WgslBufferKind {
    Uniform,
    StorageRead,
    StorageReadWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WgslResource {
    Buffer(WgslBufferKind),
    Texture,
    Sampler,
}

/// One bound global and the entry points that use it: an automatic pipeline layout holds exactly the bindings its
/// entry points use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WgslBinding {
    pub group: u32,
    pub binding: u32,
    pub resource: WgslResource,
    pub used_by: Vec<String>,
}

/// Parses and validates `source`; the bindings of every bound global, or naga's rendered error.
pub fn check_wgsl(source: &str) -> Result<Vec<WgslBinding>, String> {
    let module = naga::front::wgsl::parse_str(source).map_err(|e| e.emit_to_string(source))?;
    let info = Validator::new(ValidationFlags::all(), Capabilities::default())
        .validate(&module)
        .map_err(|e| e.emit_to_string(source))?;
    let mut bindings = Vec::new();
    for (handle, global) in module.global_variables.iter() {
        let Some(at) = &global.binding else { continue };
        let resource = match global.space {
            AddressSpace::Uniform => WgslResource::Buffer(WgslBufferKind::Uniform),
            AddressSpace::Storage { access } if access.contains(StorageAccess::STORE) => {
                WgslResource::Buffer(WgslBufferKind::StorageReadWrite)
            }
            AddressSpace::Storage { .. } => WgslResource::Buffer(WgslBufferKind::StorageRead),
            AddressSpace::Handle => match module.types[global.ty].inner {
                TypeInner::Sampler { .. } => WgslResource::Sampler,
                _ => WgslResource::Texture,
            },
            _ => continue,
        };
        let used_by = module
            .entry_points
            .iter()
            .enumerate()
            .filter(|(i, _)| !info.get_entry_point(*i)[handle].is_empty())
            .map(|(_, entry)| entry.name.clone())
            .collect();
        bindings.push(WgslBinding {
            group: at.group,
            binding: at.binding,
            resource,
            used_by,
        });
    }
    Ok(bindings)
}
