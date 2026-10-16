//! The one [`EffectDelegate`] every client-role kernel is configured with
//! (`KernelConfig.delegate`).
//!
//! `EffectExecutor` running in `RuntimeEnvironment::Client` routes every
//! server-only effect (`persist`/`fetch`/`call-service`) to the configured
//! delegate instead of executing it (`executor.rs` `do_persist`/`do_fetch`/
//! `do_call_service`). `ServerLegCollector` just accumulates what it's handed;
//! `RuntimeKernel::dispatch_with_server_leg` drains it right after one
//! `dispatch` call to decide whether that dispatch produced a server leg —
//! see `dispatch_mode.rs` for which [`super::DispatchMode`] gets one.

use orbital_core_base::effect_types::{Effect, RuntimeEnvironment};
use crate::effects::executor::EffectDelegate;
use parking_lot::Mutex;
use std::collections::{BTreeSet, HashMap};
use std::thread::ThreadId;

/// Collects effects delegated to the server environment. `drain()` takes
/// everything collected since the last drain, leaving the collector empty —
/// callers must drain once per dispatch, immediately after it, or effects
/// from an unrelated call bleed into the wrong server leg.
#[derive(Debug, Default)]
pub struct ServerLegCollector {
    effects: Mutex<Vec<Effect>>,
    /// The trait whose effects are running, per thread (a kernel may be
    /// dispatched from several threads; delegation happens on the running one).
    running: Mutex<HashMap<ThreadId, Vec<String>>>,
    /// Traits whose own effects were delegated since the last drain.
    sources: Mutex<BTreeSet<String>>,
}

struct AttributionGuard<'a> {
    collector: &'a ServerLegCollector,
    thread: ThreadId,
}

impl Drop for AttributionGuard<'_> {
    fn drop(&mut self) {
        let mut running = self.collector.running.lock();
        if let Some(stack) = running.get_mut(&self.thread) {
            stack.pop();
            if stack.is_empty() {
                running.remove(&self.thread);
            }
        }
    }
}

impl ServerLegCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attribute every effect delegated while `run` executes to `trait_name`.
    pub fn attribute<R>(&self, trait_name: &str, run: impl FnOnce() -> R) -> R {
        let thread = std::thread::current().id();
        self.running
            .lock()
            .entry(thread)
            .or_default()
            .push(trait_name.to_string());
        let _guard = AttributionGuard {
            collector: self,
            thread,
        };
        run()
    }

    /// Take every effect collected since the last drain.
    pub fn drain(&self) -> Vec<Effect> {
        self.drain_attributed().0
    }

    /// Take every effect collected since the last drain, with the traits
    /// whose own effects were delegated.
    pub fn drain_attributed(&self) -> (Vec<Effect>, BTreeSet<String>) {
        (
            std::mem::take(&mut *self.effects.lock()),
            std::mem::take(&mut *self.sources.lock()),
        )
    }
}

impl EffectDelegate for ServerLegCollector {
    fn delegate(&self, effect: &Effect, target: RuntimeEnvironment) -> Result<(), String> {
        if target.is_server() {
            self.effects.lock().push(effect.clone());
            let source = self
                .running
                .lock()
                .get(&std::thread::current().id())
                .and_then(|stack| stack.last().cloned());
            if let Some(source) = source {
                self.sources.lock().insert(source);
            }
        }
        Ok(())
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbital_core_base::value::Value;

    fn persist_effect() -> Effect {
        Effect::Persist {
            operation: orbital_core_base::effect_types::PersistOperation::Create,
            entity_type: "Note".to_string(),
            data: Value::Null,
            id: None,
            emit: None,
        }
    }

    #[test]
    fn collects_and_drains() {
        let collector = ServerLegCollector::new();
        collector
            .delegate(&persist_effect(), RuntimeEnvironment::Server)
            .unwrap();
        collector
            .delegate(&persist_effect(), RuntimeEnvironment::Server)
            .unwrap();
        let drained = collector.drain();
        assert_eq!(drained.len(), 2);
        // Drain leaves it empty for the next dispatch.
        assert!(collector.drain().is_empty());
    }

    #[test]
    fn ignores_non_server_targets() {
        let collector = ServerLegCollector::new();
        collector
            .delegate(&persist_effect(), RuntimeEnvironment::Client)
            .unwrap();
        assert!(collector.drain().is_empty());
    }

    #[test]
    fn attributes_delegated_effects_to_the_running_trait() {
        let collector = ServerLegCollector::new();
        collector.attribute("Composer", || ());
        collector.attribute("Persistor", || {
            collector
                .delegate(&persist_effect(), RuntimeEnvironment::Server)
                .unwrap();
        });
        let (effects, sources) = collector.drain_attributed();
        assert_eq!(effects.len(), 1);
        assert_eq!(
            sources.into_iter().collect::<Vec<_>>(),
            vec!["Persistor".to_string()]
        );
        assert!(collector.drain_attributed().1.is_empty());
    }

    #[test]
    fn as_any_downcasts_back_to_concrete_type() {
        let collector: std::sync::Arc<dyn EffectDelegate> =
            std::sync::Arc::new(ServerLegCollector::new());
        let concrete = collector
            .as_any()
            .and_then(|a| a.downcast_ref::<ServerLegCollector>());
        assert!(concrete.is_some());
    }
}
