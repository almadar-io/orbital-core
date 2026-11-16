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
    let (module, info) = validate(source, Capabilities::default())?;
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

/// Parses and validates `source` against what every host can run, the WebGL2 fallback included; naga's rendered
/// error when it is not.
pub fn check_wgsl_portable(source: &str) -> Result<(), String> {
    validate(source, Capabilities::empty()).map(|_| ())
}

fn validate(
    source: &str,
    capabilities: Capabilities,
) -> Result<(naga::Module, naga::valid::ModuleInfo), String> {
    let module = naga::front::wgsl::parse_str(source).map_err(|e| e.emit_to_string(source))?;
    let info = Validator::new(ValidationFlags::all(), capabilities)
        .validate(&module)
        .map_err(|e| e.emit_to_string(source))?;
    Ok((module, info))
}

#[cfg(test)]
mod tests {
    use super::{check_wgsl, check_wgsl_portable};

    const CUBE_ARRAY: &str = "@group(0) @binding(0) var t: texture_cube_array<f32>;
@group(0) @binding(1) var s: sampler;
@fragment fn main() -> @location(0) vec4<f32> { return textureSample(t, s, vec3<f32>(0.0, 0.0, 1.0), 0); }";

    const PLAIN: &str =
        "@fragment fn main() -> @location(0) vec4<f32> { return vec4<f32>(1.0, 0.0, 0.0, 1.0); }";

    #[test]
    fn the_portable_profile_refuses_what_some_host_lacks() {
        assert!(
            check_wgsl_portable(CUBE_ARRAY).is_err(),
            "cube-array textures are not on every host"
        );
        assert!(
            check_wgsl(CUBE_ARRAY).is_ok(),
            "control: the compute profile accepts them"
        );
        assert!(
            check_wgsl_portable(PLAIN).is_ok(),
            "control: a plain shader is portable"
        );
    }

    #[test]
    fn a_portable_error_names_the_source_problem() {
        let err = check_wgsl_portable("fn shade(").expect_err("a broken module");
        assert!(!err.is_empty());
    }
}
