//! A row from outside the kernel is checked once, where it arrives, against its entity's declared fields (G-CROSS-335):
//! from then on its values are trusted.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::resumable::{IoAnswer, IoRequest};
use super::row_query::RowWriteOutcome;
use super::RuntimeKernel;
use orbital_core_base::ir::types::{
    declared_alias_shape, field_shape, DeclaredShape, OirField, OirType, ShapeError,
};
use orbital_core_base::schema::types::EntityDefinition;
use orbital_core_base::value::Value;
use orbital_core_eval::evaluator::operators::arg_types::misfit_of_shape;
use std::sync::Arc;

/// An entity's row as a record of its declared fields, as the intake checks it (EXEC-EVAL-10): a present value must fit
/// its field's kind, an absent field passes and null passes only at a nullable one (a trait's working row is unsaved, so `required` is not enforced).
pub(super) fn entity_struct(entity: &EntityDefinition) -> Result<OirType, ShapeError> {
    let mut fields = Vec::new();
    for field in &entity.fields {
        let Some(name) = field.name.as_ref() else { continue };
        let mut declared = OirField::new(name.clone(), field_shape(field).map_err(|e| e.within(name.clone()))?);
        declared.required = false;
        fields.push(declared);
    }
    Ok(OirType::Struct {
        fields: fields.into(),
        additional: None,
    })
}

fn no_shape_misfit(entity_type: &str, error: &ShapeError) -> String {
    format!("store row {entity_type} cannot be checked: {entity_type} has {error}")
}

impl RuntimeKernel {
    /// The declared shape of a row of `entity_type`, closed over the entities and program types it names; `None` for
    /// an entity no registered program declares.
    pub(crate) fn row_shape(&self, entity_type: &str) -> Result<Option<Arc<DeclaredShape>>, ShapeError> {
        if let Some(shape) = self.registry.row_shapes.read().get(entity_type) {
            return Ok(shape.clone());
        }
        let shape = self.build_row_shape(entity_type)?.map(Arc::new);
        self.registry
            .row_shapes
            .write()
            .insert(entity_type.to_string(), shape.clone());
        Ok(shape)
    }

    fn build_row_shape(&self, entity_type: &str) -> Result<Option<DeclaredShape>, ShapeError> {
        let definitions = self.registry.entity_definitions.read();
        let Some(definition) = definitions.get(entity_type) else { return Ok(None) };
        let row = entity_struct(definition)?;
        let schemas = self.registry.schemas.read();
        DeclaredShape::with_resolver(
            Some(entity_type.to_string()),
            row,
            |name| {
                if let Some(entity) = definitions.get(name) {
                    return entity_struct(entity).map(Some);
                }
                schemas
                    .values()
                    .find_map(|program| {
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
                    .transpose()
            },
        )
        .map(Some)
    }

    fn row_misfit(&self, entity_type: &str, row: &Value) -> Option<String> {
        match self.row_shape(entity_type) {
            Ok(Some(shape)) => row_misfit_against(entity_type, &shape, row),
            Ok(None) => None,
            Err(error) => Some(no_shape_misfit(entity_type, &error)),
        }
    }

    /// Why one of `rows` does not fit `entity_type`'s declaration, when one does not.
    pub(crate) fn rows_misfit<'a>(
        &self,
        entity_type: &str,
        rows: impl IntoIterator<Item = &'a Value>,
    ) -> Option<String> {
        rows.into_iter().find_map(|row| self.row_misfit(entity_type, row))
    }

    /// [`Self::rows_misfit`] for rows a store creates: one without an `id` gets the id the store assigns, so its
    /// absence is not a misfit.
    pub(crate) fn created_rows_misfit(&self, entity_type: &str, rows: &[Value]) -> Option<String> {
        let created: Vec<std::borrow::Cow<'_, Value>> = rows
            .iter()
            .map(|row| match row {
                Value::Object(fields) if !fields.contains_key("id") => {
                    let mut fields = fields.clone();
                    fields.insert("id".into(), Value::string(String::new()));
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
        match answer {
            IoAnswer::Row(Ok(row)) => {
                let misfit = entity_type.and_then(|entity_type| self.row_misfit(entity_type, &row));
                IoAnswer::Row(misfit.map_or(Ok(row), Err))
            }
            IoAnswer::Rows(Ok(rows)) => {
                let misfit = entity_type
                    .and_then(|entity_type| rows.iter().find_map(|row| self.row_misfit(entity_type, row)));
                IoAnswer::Rows(misfit.map_or(Ok(rows), Err))
            }
            IoAnswer::Page(Ok(page)) => {
                let misfit = entity_type
                    .and_then(|entity_type| page.rows.iter().find_map(|row| self.row_misfit(entity_type, row)));
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
                            self.row_misfit(entity_type, row)
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

#[cfg(test)]
mod shape_tests {
    use super::*;

    fn entity(fields: serde_json::Value) -> EntityDefinition {
        serde_json::from_value(serde_json::json!({ "name": "Note", "fields": fields })).unwrap()
    }

    #[test]
    fn an_entity_field_with_no_shape_is_a_typed_error_naming_it() {
        let shapeless = entity(serde_json::json!([
            { "name": "id", "type": "string" },
            { "name": "meta", "type": "object" }
        ]));
        let error = entity_struct(&shapeless).unwrap_err();
        assert_eq!(error.path, vec!["meta".to_string()]);
        assert!(no_shape_misfit("Note", &error).contains("Note has no declared shape at `meta`"), "{error}");
        let shaped = entity(serde_json::json!([
            { "name": "id", "type": "string" },
            { "name": "meta", "type": "object", "properties": { "tag": { "type": "string" } } }
        ]));
        assert!(entity_struct(&shaped).is_ok(), "control: a declared record has a shape");
    }
}
