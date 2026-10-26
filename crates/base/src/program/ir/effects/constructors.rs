//! One constructor per effect.

use super::*;

impl OirEffect {
    /// Create a set effect
    pub fn set(target: OirBinding, value: OirExpr) -> Self {
        Self::Set {
            target,
            value,
            emit: None,
        }
    }

    /// Create a set effect with a dynamic path (computed at runtime)
    pub fn set_dynamic(target: OirExpr, value: OirExpr) -> Self {
        Self::SetDynamic {
            target,
            value,
            emit: None,
        }
    }

    /// Create an emit effect
    pub fn emit(event: impl Into<String>) -> Self {
        Self::Emit {
            event: event.into(),
            payload: None,
        }
    }

    /// Create an emit effect with payload
    pub fn emit_with_payload(event: impl Into<String>, payload: OirExpr) -> Self {
        Self::Emit {
            event: event.into(),
            payload: Some(payload),
        }
    }

    /// Create a fetch effect (list all)
    pub fn fetch(entity_type: impl Into<String>) -> Self {
        Self::Fetch {
            entity_type: entity_type.into(),
            id: None,
            filter: None,
            order_by: None,
            offset: None,
            limit: None,
            include: None,
            nearest: None,
            emit: None,
        }
    }

    /// Create a fetch effect by ID (single entity)
    pub fn fetch_by_id(entity_type: impl Into<String>, id: OirExpr) -> Self {
        Self::Fetch {
            entity_type: entity_type.into(),
            id: Some(id),
            filter: None,
            order_by: None,
            offset: None,
            limit: None,
            include: None,
            nearest: None,
            emit: None,
        }
    }

    /// Create a fetch effect with filter
    pub fn fetch_with_filter(entity_type: impl Into<String>, filter: OirExpr) -> Self {
        Self::Fetch {
            entity_type: entity_type.into(),
            id: None,
            filter: Some(filter),
            order_by: None,
            offset: None,
            limit: None,
            include: None,
            nearest: None,
            emit: None,
        }
    }

    /// Create a fetch effect with filter, sort, and limit
    pub fn fetch_with_options(
        entity_type: impl Into<String>,
        filter: Option<OirExpr>,
        order_by: Option<OirExpr>,
        limit: Option<OirExpr>,
    ) -> Self {
        Self::Fetch {
            entity_type: entity_type.into(),
            id: None,
            filter,
            order_by,
            offset: None,
            limit,
            include: None,
            nearest: None,
            emit: None,
        }
    }

    // ── Resource operator constructors ──

    /// Create a ref effect (reactive entity subscription)
    pub fn ref_entity(entity_type: impl Into<String>) -> Self {
        Self::Ref {
            entity_type: entity_type.into(),
            id: None,
            filter: None,
            order_by: None,
            limit: None,
            include: None,
            emit: None,
        }
    }

    /// Create a deref effect (snapshot read)
    pub fn deref_entity(entity_type: impl Into<String>) -> Self {
        Self::Deref {
            entity_type: entity_type.into(),
            id: None,
        }
    }

    /// Create a deref effect with ID (single-record read)
    pub fn deref_by_id(entity_type: impl Into<String>, id: OirExpr) -> Self {
        Self::Deref {
            entity_type: entity_type.into(),
            id: Some(id),
        }
    }

    /// Create a swap effect (atomic CAS)
    pub fn swap(entity_type: impl Into<String>, id: OirExpr, transform: OirExpr) -> Self {
        Self::Swap {
            entity_type: entity_type.into(),
            id,
            transform,
        }
    }

    /// Create a watch effect (react to entity changes)
    pub fn watch(entity_type: impl Into<String>, effects: Vec<OirExpr>) -> Self {
        Self::Watch {
            entity_type: entity_type.into(),
            effects,
        }
    }

    /// Create an atomic effect (transaction group)
    pub fn atomic(effects: Vec<OirExpr>) -> Self {
        Self::Atomic { effects }
    }

    /// Create a persist batch effect
    pub fn persist_batch(ops: OirExpr, entities: Vec<String>) -> Self {
        Self::PersistBatch {
            ops,
            entities,
            emit: None,
        }
    }

    /// Create a persist create effect
    pub fn create(entity_type: impl Into<String>, data: OirExpr) -> Self {
        Self::Persist {
            operation: OirPersistOp::Create,
            entity_type: entity_type.into(),
            data,
            emit: None,
        }
    }

    /// Create a persist upsert effect (create-or-replace by id)
    pub fn upsert(entity_type: impl Into<String>, data: OirExpr) -> Self {
        Self::Persist {
            operation: OirPersistOp::Upsert,
            entity_type: entity_type.into(),
            data,
            emit: None,
        }
    }

    /// Create a persist update effect
    pub fn update(entity_type: impl Into<String>, data: OirExpr) -> Self {
        Self::Persist {
            operation: OirPersistOp::Update,
            entity_type: entity_type.into(),
            data,
            emit: None,
        }
    }

    /// Create a persist delete effect
    pub fn delete(entity_type: impl Into<String>, data: OirExpr) -> Self {
        Self::Persist {
            operation: OirPersistOp::Delete,
            entity_type: entity_type.into(),
            data,
            emit: None,
        }
    }

    /// Create a persist read effect
    pub fn read(entity_type: impl Into<String>, data: OirExpr) -> Self {
        Self::Persist {
            operation: OirPersistOp::Read,
            entity_type: entity_type.into(),
            data,
            emit: None,
        }
    }

    /// Create a persist list effect
    pub fn list(entity_type: impl Into<String>, data: OirExpr) -> Self {
        Self::Persist {
            operation: OirPersistOp::List,
            entity_type: entity_type.into(),
            data,
            emit: None,
        }
    }

    /// Create a persist query effect
    pub fn query(entity_type: impl Into<String>, data: OirExpr) -> Self {
        Self::Persist {
            operation: OirPersistOp::Query,
            entity_type: entity_type.into(),
            data,
            emit: None,
        }
    }

    /// Create a navigate effect
    pub fn navigate(path: OirExpr) -> Self {
        Self::Navigate {
            path,
            params: None,
            crumb: None,
        }
    }

    /// Create a navigate-back effect
    pub fn navigate_back() -> Self {
        Self::NavigateBack
    }

    /// Create a render-ui effect
    pub fn render_ui(slot: impl Into<String>, pattern: OirPattern) -> Self {
        Self::RenderUi {
            slot: slot.into(),
            pattern,
        }
    }

    /// Create a log effect
    pub fn log(level: OirLogLevel, message: OirExpr) -> Self {
        Self::Log { level, message }
    }

    /// Create a spawn effect
    pub fn spawn(entity_type: impl Into<String>, props: OirExpr) -> Self {
        Self::Spawn {
            entity_type: entity_type.into(),
            props,
        }
    }

    /// Create a despawn effect
    pub fn despawn(entity_id: OirExpr) -> Self {
        Self::Despawn { entity_id }
    }

    /// Create a call-service effect
    pub fn call_service(
        service: impl Into<String>,
        method: impl Into<String>,
        args: OirExpr,
    ) -> Self {
        Self::CallService {
            service: service.into(),
            method: method.into(),
            args,
            key: None,
            emit: None,
        }
    }

    /// Create an http effect
    pub fn http(method: HttpMethod, origin: OirExpr, path: OirExpr) -> Self {
        Self::Http {
            method,
            origin,
            path,
            request: None,
            key: None,
            emit: None,
        }
    }

    /// Create a redis effect
    pub fn redis(origin: OirExpr, command: OirExpr) -> Self {
        Self::Redis {
            origin,
            command,
            options: None,
            key: None,
            emit: None,
        }
    }

    /// Create a postgres effect
    pub fn postgres(origin: OirExpr, sql: OirExpr, params: OirExpr) -> Self {
        Self::Postgres {
            origin,
            sql,
            params,
            options: None,
            key: None,
            emit: None,
        }
    }

    /// Create a cancel-call effect
    pub fn cancel_call(key: OirExpr) -> Self {
        Self::CancelCall { key }
    }

    /// Create a random-bytes effect
    pub fn random_bytes(bytes: OirExpr, emit: Option<OirEmitConfig>) -> Self {
        Self::RandomBytes {
            bytes,
            key: None,
            emit,
        }
    }

    /// Create an `os/<op>` effect
    pub fn os(
        op: OsOp,
        args: Vec<OirExpr>,
        key: Option<OirExpr>,
        emit: Option<OirEmitConfig>,
    ) -> Self {
        Self::Os {
            op,
            args,
            key,
            emit,
        }
    }

    /// Create a credential-status effect
    pub fn credential_status(credential: OirExpr, emit: Option<OirEmitConfig>) -> Self {
        Self::CredentialStatus { credential, emit }
    }

    /// Create a credential-forget effect
    pub fn credential_forget(credential: OirExpr, emit: Option<OirEmitConfig>) -> Self {
        Self::CredentialForget { credential, emit }
    }

    /// Create a credential administration effect
    pub fn credential_admin(
        op: crate::credential_admin::CredentialAdminOp,
        request: OirExpr,
        emit: Option<OirEmitConfig>,
    ) -> Self {
        Self::CredentialAdmin { op, request, emit }
    }

    /// Create an access-check effect
    pub fn access_check(request: OirExpr, emit: Option<OirEmitConfig>) -> Self {
        Self::AccessCheck { request, emit }
    }
}
