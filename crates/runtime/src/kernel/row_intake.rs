//! A row from outside the kernel is checked once, where it arrives, against its entity's declared fields (G-CROSS-335):
//! from then on its values are trusted.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::resumable::{IoAnswer, IoRequest};
use super::row_query::RowWriteOutcome;
use super::RuntimeKernel;
use orbital_core_base::ir::types::{
    declared_alias_shape, field_shape, DeclaredShape, OirField, OirType,
};
use orbital_core_base::schema::types::EntityDefinition;
use orbital_core_base::value::Value;
use orbital_core_eval::evaluator::operators::arg_types::misfit_of_shape;
use std::sync::Arc;

/// An entity's row as a record of its declared fields.
pub(super) fn entity_struct(entity: &EntityDefinition) -> OirType {
    let fields = entity
        .fields
        .iter()
        .filter_map(|field| {
            let name = field.name.as_ref()?;
            let mut declared = OirField::new(name.clone(), field_shape(field));
            declared.required = field.required.unwrap_or(false);
            Some(declared)
        })
        .collect();
    OirType::Struct {
        fields,
        additional: None,
    }
}

impl RuntimeKernel {
    /// The declared shape of a row of `entity_type`, closed over the entities and program types it names; `None` for
    /// an entity no registered program declares.
    pub(crate) fn row_shape(&self, entity_type: &str) -> Option<Arc<DeclaredShape>> {
        if let Some(shape) = self.registry.row_shapes.read().get(entity_type) {
            return shape.clone();
        }
        let shape = self.build_row_shape(entity_type).map(Arc::new);
        self.registry
            .row_shapes
            .write()
            .insert(entity_type.to_string(), shape.clone());
        shape
    }

    fn build_row_shape(&self, entity_type: &str) -> Option<DeclaredShape> {
        let definitions = self.registry.entity_definitions.read();
        let row = entity_struct(definitions.get(entity_type)?);
        let schemas = self.registry.schemas.read();
        Some(DeclaredShape::with_resolver(
            Some(entity_type.to_string()),
            row,
            |name| {
                if let Some(entity) = definitions.get(name) {
                    return Some(entity_struct(entity));
                }
                schemas.values().find_map(|program| {
                    program
                        .types
                        .iter()
                        .chain(
                            program
                                .orbitals
                                .iter()
                                .filter_map(|orbital| orbital.types.as_ref()),
                        )
                        .find_map(|aliases| aliases.get(name))
                        .map(|members| declared_alias_shape(members))
                })
            },
        ))
    }

    /// Why one of `rows` does not fit `entity_type`'s declaration, when one does not.
    pub(crate) fn rows_misfit<'a>(
        &self,
        entity_type: &str,
        rows: impl IntoIterator<Item = &'a Value>,
    ) -> Option<String> {
        let shape = self.row_shape(entity_type)?;
        rows.into_iter()
            .find_map(|row| row_misfit_against(entity_type, &shape, row))
    }

    /// [`Self::rows_misfit`] for rows a store creates: one without an `id` gets the id the store assigns, so its
    /// absence is not a misfit.
    pub(crate) fn created_rows_misfit(&self, entity_type: &str, rows: &[Value]) -> Option<String> {
        let created: Vec<std::borrow::Cow<'_, Value>> = rows
            .iter()
            .map(|row| match row {
                Value::Object(fields) if !fields.contains_key("id") => {
                    let mut fields = fields.clone();
                    fields.insert("id".to_string(), Value::string(String::new()));
                    std::borrow::Cow::Owned(Value::Object(fields))
                }
                _ => std::borrow::Cow::Borrowed(row),
            })
            .collect();
        self.rows_misfit(entity_type, created.iter().map(|row| row.as_ref()))
    }

    /// `answer` to `request`, with a row that does not fit its entity's declaration turned into the store failure
    /// the asking effect answers.
    pub(crate) fn checked_answer(&self, request: &IoRequest, answer: IoAnswer) -> IoAnswer {
        let entity_type = match request {
            IoRequest::ReadRow { entity_type, .. }
            | IoRequest::ListRows { entity_type, .. }
            | IoRequest::QueryRows { entity_type, .. }
            | IoRequest::CreateRow { entity_type, .. }
            | IoRequest::WriteRow { entity_type, .. } => Some(entity_type.as_str()),
            _ => None,
        };
        let shape = |entity_type: &str| self.row_shape(entity_type);
        match answer {
            IoAnswer::Row(Ok(row)) => {
                let misfit = entity_type
                    .and_then(|entity_type| Some((entity_type, shape(entity_type)?)))
                    .and_then(|(entity_type, shape)| row_misfit_against(entity_type, &shape, &row));
                IoAnswer::Row(misfit.map_or(Ok(row), Err))
            }
            IoAnswer::Rows(Ok(rows)) => {
                let misfit = entity_type
                    .and_then(|entity_type| Some((entity_type, shape(entity_type)?)))
                    .and_then(|(entity_type, shape)| {
                        rows.iter()
                            .find_map(|row| row_misfit_against(entity_type, &shape, row))
                    });
                IoAnswer::Rows(misfit.map_or(Ok(rows), Err))
            }
            IoAnswer::Page(Ok(page)) => {
                let misfit = entity_type
                    .and_then(|entity_type| Some((entity_type, shape(entity_type)?)))
                    .and_then(|(entity_type, shape)| {
                        page.rows
                            .iter()
                            .find_map(|row| row_misfit_against(entity_type, &shape, row))
                    });
                IoAnswer::Page(misfit.map_or(Ok(page), Err))
            }
            IoAnswer::Wrote(Ok(outcomes)) => {
                let misfit = match request {
                    IoRequest::WriteRows { batch } => {
                        batch.ops.iter().zip(&outcomes).find_map(|(op, outcome)| {
                            let entity_type = op.entity_type();
                            let row = match outcome {
                                RowWriteOutcome::Created { row }
                                | RowWriteOutcome::Updated { row, .. } => row,
                                _ => return None,
                            };
                            let shape = shape(entity_type)?;
                            row_misfit_against(entity_type, &shape, row)
                        })
                    }
                    _ => None,
                };
                IoAnswer::Wrote(misfit.map_or(Ok(outcomes), Err))
            }
            other => other,
        }
    }
}

fn row_misfit_against(entity_type: &str, shape: &DeclaredShape, row: &Value) -> Option<String> {
    if row.is_null() {
        return None;
    }
    let (path, actual) = misfit_of_shape(row, shape)?;
    let id = row.get_path("id").and_then(Value::as_str).unwrap_or("");
    Some(format!(
        "store row {entity_type} {id:?} does not fit its declaration: {entity_type}{path} is {actual}"
    ))
}
