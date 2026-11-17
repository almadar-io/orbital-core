//! EXEC-CONC-1a: a viewer dispatch's deliveries to distinct keyed rows of a boot trait park only their own row on a store
//! read, so their reads are in flight together; deliveries to an unkeyed boot trait still run one after another.

use orbital_core_base::effect_types::RuntimeEnvironment;
use orbital_core_base::json_value::from_json_value;
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::sync::{Condvar, Mutex};
use orbital_core_base::value::Value;
use orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache};
use orbital_core_runtime::kernel::RuntimeKernel;
use orbital_core_runtime::os_host::completion::KernelCompletionSink;
use orbital_core_runtime::resumable::DispatchOutcomes;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

const SETTLE: Duration = Duration::from_secs(3);

fn program(keyed: bool) -> OrbitalSchema {
    program_reading(keyed, false)
}

/// `twice`: each row reads again once its first read is answered.
fn program_reading(keyed: bool, twice: bool) -> OrbitalSchema {
    let mut listen = json!({ "event": "ASK", "triggers": "WORK", "source": { "kind": "trait", "trait": "Desk" } });
    if keyed {
        listen["key"] = json!("@payload.key");
    }
    let route = json!({ "success": "DRV_GOT", "failure": "DRV_FAILED" });
    let got = if twice {
        json!({ "from": "reading", "to": "again", "event": "GOT", "effects": [["fetch", "Ticket", { "id": "t2", "emit": { "success": "GOT2", "failure": "MISSED" } }]] })
    } else {
        json!({ "from": "reading", "to": "idle", "event": "GOT", "effects": [["set", "@entity.note", "got"]] })
    };
    from_json_value(&json!({ "name": "overlap", "typeContractVersion": 1, "orbitals": [
        { "name": "Tickets",
          "entity": { "name": "Ticket", "persistence": "persistent", "store": "Rows",
              "fields": [{ "name": "id", "type": "string" }, { "name": "used", "type": "boolean" }] },
          "traits": [], "pages": [] },
        { "name": "Work",
          "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "note", "type": "string", "default": "" }] },
          "traits": [{ "name": "Desk", "linkedEntity": "Job",
              "emits": [{ "event": "ASK", "scope": "external", "payloadSchema": [{ "name": "key", "type": "string" }] }],
              "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [
                  { "from": "idle", "to": "idle", "event": "GO", "effects": [["emit", "ASK", { "key": "k1" }], ["emit", "ASK", { "key": "k2" }]] }] } },
            { "name": "Worker", "linkedEntity": "Job", "listens": [listen],
              "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "reading" }, { "name": "again" }], "transitions": [
                  { "from": "idle", "to": "reading", "event": "WORK", "effects": [["fetch", "Ticket", { "id": "t1", "emit": { "success": "GOT", "failure": "MISSED" } }]] },
                  got,
                  { "from": "again", "to": "idle", "event": "GOT2", "effects": [["set", "@entity.note", "got twice"]] },
                  { "from": "reading", "to": "idle", "event": "MISSED", "effects": [["set", "@entity.note", "missed"]] }] } }],
          "pages": [{ "name": "Home", "path": "/", "traits": [{ "ref": "Desk" }] },
                    { "name": "Work", "path": "", "protocol": "boot", "traits": [{ "ref": "Worker" }] }] },
        { "name": "Rows",
          "entity": { "name": "Mirror", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
          "traits": [{ "name": "RowsDriver", "linkedEntity": "Mirror",
              "stateMachine": { "states": [{ "name": "ready", "isInitial": true }],
                  "events": [{ "key": "ROW_READ", "name": "ROW_READ" }, { "key": "HOLD", "name": "HOLD", "input": true }],
                  "transitions": [
                      { "from": "ready", "to": "ready", "event": "ROW_READ",
                        "effects": [["program/dispatch", "Rows.RowsDriver.HOLD", {}, { "key": "@payload.key", "emit": route }]] },
                      { "from": "ready", "to": "ready", "event": "DRV_GOT",
                        "effects": [["emit", "ROW_FOUND", { "key": "@payload.key", "row": { "id": "t1", "used": false } }]] },
                      { "from": "ready", "to": "ready", "event": "DRV_FAILED",
                        "effects": [["emit", "ROW_FOUND", { "key": "@payload.key" }]] }] } }],
          "pages": [{ "name": "Rows", "path": "", "protocol": "boot", "traits": [{ "ref": "RowsDriver" }] }] }
    ] }))
    .unwrap()
}

/// The store's I/O: each read enters, then waits until the test opens the gate.
#[derive(Default)]
struct Gate {
    entered: Mutex<usize>,
    permits: Mutex<usize>,
    changed: Condvar,
    opened: Condvar,
}

impl Gate {
    fn pass(&self) {
        *self.entered.lock() += 1;
        self.changed.notify_all();
        let mut permits = self.permits.lock();
        while *permits == 0 {
            self.opened.wait(&mut permits);
        }
        *permits -= 1;
    }

    /// The reads in flight together once `n` have entered within `within`, or the count reached.
    fn entered_within(&self, n: usize, within: Duration) -> usize {
        let deadline = std::time::Instant::now() + within;
        let mut entered = self.entered.lock();
        while *entered < n {
            let now = std::time::Instant::now();
            if now >= deadline {
                break;
            }
            self.changed.wait_for(&mut entered, deadline - now);
        }
        *entered
    }

    fn release(&self) {
        self.permit(usize::MAX / 2);
    }

    fn permit(&self, n: usize) {
        *self.permits.lock() += n;
        self.opened.notify_all();
    }
}

struct Run {
    gate: Arc<Gate>,
    done: std::sync::mpsc::Receiver<DispatchOutcomes>,
}

fn go(keyed: bool) -> Run {
    go_reading(keyed, false)
}

fn go_reading(keyed: bool, twice: bool) -> Run {
    let gate = Arc::new(Gate::default());
    let config = orbital_core_runtime::config::KernelConfig {
        substrate_handler: Some(Arc::new(orbital_core_runtime::resumable::HostAnswered)
            as Arc<dyn orbital_core_runtime::effects::SubstrateHandler>),
        ..Default::default()
    };
    let store = Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>;
    let kernel = Arc::new(RuntimeKernel::new(
        RuntimeEnvironment::Server,
        store.clone(),
        config,
    ));
    kernel
        .register_schema(program_reading(keyed, twice))
        .unwrap();
    let handles = Arc::new(orbital_core_runtime::os_host::OsHandles::default());
    let native = Arc::new(
        orbital_core_runtime::native_host::NativeHost::new(
            store,
            Arc::new(orbital_core_runtime::credentials::Credentials::from_env(
                Default::default(),
            )),
            orbital_core_runtime::os_host::OsGrants::default(),
            handles.clone(),
            Arc::new(|_: Option<&orbital_core_runtime::resumable::CallSite>, _: Value| {}),
        )
        .with_substrate_later(Some({
            let gate = gate.clone();
            Arc::new(move |_: &str, _: &[Value]| {
                let gate = gate.clone();
                Box::new(move || {
                    gate.pass();
                    Ok(from_json_value(&json!({ "ok": true })).unwrap())
                }) as orbital_core_runtime::native_host::SubstrateWork
            }) as Arc<orbital_core_runtime::native_host::SubstrateLater>
        })),
    );
    let sink =
        Arc::new(
            KernelCompletionSink::new(
                kernel.clone(),
                Arc::new(
                    move |request: &orbital_core_runtime::resumable::IoRequest| {
                        native.answer(request)
                    },
                ),
                Arc::new(
                    |_: &orbital_core_runtime::resumable::CallSite,
                     _: &str,
                     _: &Value,
                     _: Option<&str>,
                     _: Result<
                        DispatchOutcomes,
                        orbital_core_runtime::resumable::DispatchError,
                    >| {},
                ),
            ),
        );
    handles.completions.set_sink(sink.clone());
    let (tx, done) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _viewer = kernel.enter_session("tab-a");
        let progress = kernel
            .start_dispatch("Desk", None, "GO", Value::Null, None, None)
            .unwrap();
        if let Ok(outcomes) = sink.drive(progress) {
            let _ = tx.send(outcomes);
        }
    });
    Run { gate, done }
}

fn worker_answers(outcomes: &DispatchOutcomes) -> usize {
    outcomes
        .iter()
        .filter(|(t, r)| t == "Worker" && r.executed && r.event.as_deref() == Some("GOT"))
        .count()
}

#[test]
fn two_keyed_rows_store_reads_are_in_flight_together() {
    let run = go(true);
    let together = run.gate.entered_within(2, SETTLE);
    run.gate.release();
    assert_eq!(
        together, 2,
        "the second keyed row's read waited for the first to be answered"
    );
    let outcomes = run.done.recv_timeout(SETTLE).expect("GO ran to its end");
    assert_eq!(
        worker_answers(&outcomes),
        2,
        "each row's fetch answered on its own route"
    );
}

#[test]
fn control_an_unkeyed_boot_traits_deliveries_still_read_one_after_another() {
    let run = go(false);
    let first = run.gate.entered_within(2, Duration::from_millis(500));
    run.gate.release();
    assert_eq!(
        first, 1,
        "an unkeyed trait's second delivery started before the first was answered"
    );
    run.done.recv_timeout(SETTLE).expect("GO ran to its end");
}

/// The same program on a host with no completion sink, its driver answering in its own step: nothing defers.
fn answered_in_place(keyed: bool) -> DispatchOutcomes {
    let mut schema = serde_json::to_value(program(keyed)).unwrap();
    let driver = &mut schema["orbitals"][2]["traits"][0]["stateMachine"]["transitions"][0];
    driver["effects"] = json!([["emit", "ROW_FOUND", { "key": "@payload.key", "row": { "id": "t1", "used": false } }]]);
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    kernel
        .register_schema(from_json_value(&schema).unwrap())
        .unwrap();
    let rows = InMemoryEntityCache::new();
    let _viewer = kernel.enter_session("tab-a");
    let progress = kernel
        .start_dispatch("Desk", None, "GO", Value::Null, None, None)
        .unwrap();
    orbital_core_runtime::resumable::drive_blocking(&kernel, progress, &|request| {
        orbital_core_runtime::resumable::BlockingHost {
            source: &rows,
            service: None,
            substrate: None,
        }
        .answer(request)
    })
    .unwrap()
}

#[test]
fn without_a_completion_sink_both_keyed_rows_answer_in_delivery_order() {
    let outcomes = answered_in_place(true);
    let order: Vec<_> = outcomes
        .iter()
        .filter(|(t, r)| t == "Worker" && r.executed)
        .map(|(_, r)| r.event.clone().unwrap_or_default())
        .collect();
    assert_eq!(order, vec!["WORK", "GOT", "WORK", "GOT"], "{order:?}");
}

#[test]
fn control_without_a_completion_sink_an_unkeyed_trait_answers_the_same() {
    assert_eq!(worker_answers(&answered_in_place(false)), 2);
}

#[test]
fn two_keyed_rows_second_reads_are_in_flight_together_too() {
    let run = go_reading(true, true);
    assert_eq!(run.gate.entered_within(2, SETTLE), 2, "the first reads");
    run.gate.permit(2);
    let together = run.gate.entered_within(4, SETTLE);
    run.gate.release();
    assert_eq!(
        together, 4,
        "one row's second read waited for the other row's"
    );
    let outcomes = run.done.recv_timeout(SETTLE).expect("GO ran to its end");
    let twice = outcomes
        .iter()
        .filter(|(t, r)| t == "Worker" && r.executed && r.event.as_deref() == Some("GOT2"))
        .count();
    assert_eq!(twice, 2, "each row answered its second read");
}
