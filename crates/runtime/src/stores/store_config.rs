//! The row store the environment selects: `DATA_BACKEND` (`mock` | `firebase` | `postgres` | `couchdb`, default
//! `firebase`), the `USE_MOCK_DATA` flag, `DATABASE_URL`, `PGPOOL_MAX`, `COUCHDB_URL` and the Firebase variables,
//! the contract `@almadar/db`'s `persistenceSpecFromEnv` reads. A native host asks it for the store its kernel answers
//! row requests from, so the server, the Node add-on and the Python extension select a store one way.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::couchdb::CouchRows;
use crate::entity_source::{EntitySource, InMemoryEntityCache};
use crate::firestore::{FirestoreConfig, FirestoreRows};
use crate::http_service::Transport;
use crate::pg_rows::PostgresRows;
use crate::row_query::{QueryCapabilities, WriteCapabilities};
use std::sync::Arc;

/// Which backend the environment selects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistenceSpec {
    Mock,
    Firestore,
    Postgres {
        connection_string: String,
        pool_max: Option<u32>,
    },
    CouchDb {
        url: String,
    },
}

/// A store the kernel answers row requests from, with the push-down it declares (`None`: the kernel does the full-table
/// path itself).
pub struct RowStore {
    pub source: Arc<dyn EntitySource>,
    pub query: Option<QueryCapabilities>,
    pub write: Option<WriteCapabilities>,
}

/// The server env contract as a [`PersistenceSpec`]. Mock is refused under `NODE_ENV=production` (an unset
/// `NODE_ENV` counts as production): its rows are fabricated and lost on restart.
pub fn persistence_spec_from_env(
    get: &dyn Fn(&str) -> Option<String>,
) -> Result<PersistenceSpec, String> {
    let node_env = get("NODE_ENV").unwrap_or_else(|| "production".to_string());
    let flag = get("USE_MOCK_DATA").unwrap_or_else(|| "false".to_string());
    if flag != "true" && flag != "false" {
        return Err(format!(
            "@almadar/db: USE_MOCK_DATA must be 'true' or 'false', got '{flag}'"
        ));
    }
    let use_mock = flag == "true";
    let backend = get("DATA_BACKEND").unwrap_or_else(|| "firebase".to_string());
    if !["mock", "firebase", "postgres", "couchdb"].contains(&backend.as_str()) {
        return Err(format!("@almadar/db: DATA_BACKEND must be one of mock, firebase, postgres, couchdb, got '{backend}'"));
    }
    if node_env == "production" && use_mock {
        return Err("@almadar/server: USE_MOCK_DATA=true is not permitted when NODE_ENV=production. Unset USE_MOCK_DATA to use the real data source, or set NODE_ENV=development.".to_string());
    }
    if node_env == "production" && backend == "mock" {
        return Err("@almadar/server: DATA_BACKEND=mock is not permitted when NODE_ENV=production. Set DATA_BACKEND=postgres or 'firebase', or set NODE_ENV=development.".to_string());
    }
    if use_mock || backend == "mock" {
        return Ok(PersistenceSpec::Mock);
    }
    match backend.as_str() {
        "postgres" => {
            let connection_string = get("DATABASE_URL")
                .filter(|s| !s.is_empty())
                .ok_or("@almadar/server: DATA_BACKEND=postgres requires DATABASE_URL to be set")?;
            let pool_max = get("PGPOOL_MAX").and_then(|s| s.parse().ok());
            Ok(PersistenceSpec::Postgres {
                connection_string,
                pool_max,
            })
        }
        "couchdb" => {
            let url = get("COUCHDB_URL")
                .filter(|s| !s.is_empty())
                .ok_or("@almadar/server: DATA_BACKEND=couchdb requires COUCHDB_URL to be set")?;
            Ok(PersistenceSpec::CouchDb { url })
        }
        _ => Ok(PersistenceSpec::Firestore),
    }
}

/// The store driver `spec` names, speaking over `transport`; `None` for [`PersistenceSpec::Mock`], whose rows live in
/// the caller's own memory.
pub fn driver_store(
    spec: &PersistenceSpec,
    get: &dyn Fn(&str) -> Option<String>,
    read_file: &dyn Fn(&str) -> Result<String, String>,
    transport: Arc<dyn Transport>,
) -> Result<Option<RowStore>, String> {
    Ok(match spec {
        PersistenceSpec::Mock => None,
        PersistenceSpec::Firestore => {
            let config = FirestoreConfig::from_lookup(get, read_file, transport.clone())?;
            let rows = FirestoreRows::new(config, transport, "", Vec::new())?;
            let (query, write) = (rows.query_capabilities(), rows.write_capabilities());
            Some(RowStore {
                source: Arc::new(rows),
                query: Some(query),
                write: Some(write),
            })
        }
        PersistenceSpec::Postgres {
            connection_string, ..
        } => {
            let rows = PostgresRows::from_url(connection_string)?;
            let (query, write) = (rows.query_capabilities(), rows.write_capabilities());
            Some(RowStore {
                source: Arc::new(rows),
                query: Some(query),
                write: Some(write),
            })
        }
        PersistenceSpec::CouchDb { url } => {
            let rows = CouchRows::new(url, transport)?;
            let (query, write) = (rows.query_capabilities(), rows.write_capabilities());
            Some(RowStore {
                source: Arc::new(rows),
                query: Some(query),
                write: Some(write),
            })
        }
    })
}

/// The row store the environment selects: the in-memory store for mock data, else the named driver.
pub fn row_store_from_env(
    get: &dyn Fn(&str) -> Option<String>,
    read_file: &dyn Fn(&str) -> Result<String, String>,
    transport: Arc<dyn Transport>,
) -> Result<RowStore, String> {
    let spec = persistence_spec_from_env(get)?;
    Ok(
        driver_store(&spec, get, read_file, transport)?.unwrap_or_else(|| RowStore {
            source: Arc::new(InMemoryEntityCache::new()),
            query: None,
            write: None,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_service::{HttpRequest, HttpResponse, TransportError};

    struct Dead;

    impl Transport for Dead {
        fn send(&self, _: &HttpRequest) -> Result<HttpResponse, TransportError> {
            Err(TransportError::Failed("no network in this test".into()))
        }
    }

    fn env(pairs: Vec<(&'static str, &'static str)>) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    fn no_file(_: &str) -> Result<String, String> {
        Err("no file".into())
    }

    #[test]
    fn mock_data_is_the_in_memory_store_with_no_push_down() {
        let store = row_store_from_env(
            &env(vec![("DATA_BACKEND", "mock"), ("NODE_ENV", "development")]),
            &no_file,
            Arc::new(Dead),
        )
        .unwrap();
        assert!(store.query.is_none() && store.write.is_none());
        let row = store
            .source
            .create("Task", &serde_json::from_str(r#"{"title":"a"}"#).unwrap())
            .unwrap();
        assert_eq!(store.source.list("Task").unwrap().len(), 1, "{row:?}");
    }

    #[test]
    fn control_the_postgres_driver_declares_its_push_down_and_production_refuses_mock() {
        let store = row_store_from_env(
            &env(vec![
                ("DATA_BACKEND", "postgres"),
                ("DATABASE_URL", "postgres://u:p@127.0.0.1:1/db"),
            ]),
            &no_file,
            Arc::new(Dead),
        )
        .unwrap();
        assert!(store.write.is_some_and(|w| w.atomic_batch));
        assert!(row_store_from_env(
            &env(vec![("DATA_BACKEND", "mock")]),
            &no_file,
            Arc::new(Dead)
        )
        .is_err());
    }
}
