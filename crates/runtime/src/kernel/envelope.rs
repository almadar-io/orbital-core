//! Cross-unit delivery of an `emit` (EXEC-QUEUE-9, `docs/Almadar_Sandbox.md` Contracts 1). With a `unit` and a
//! `deployment`, an `emit` whose declared listener is placed in another unit leaves the local worklist as an
//! [`IoRequest::Deliver`] ask; a refusal comes back to the emitter as `DELIVERY_REJECTED`. The inbound side judges
//! an [`Envelope`] and runs it as a fresh dispatch carrying its hop and causal id.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::*;
use crate::listener::ListenerInfo;
use crate::resumable::DeliverAsk;
use orbital_core_base::envelope::{
    envelope_id, Accepted, DeliverRefusal, DeliverRefusalKind, DeliveryRefused, Envelope,
    EnvelopeVerdict, InstanceRef, DELIVERY_REJECTED, HOP_CAP,
};
use orbital_core_base::schema::sexpr::SObject;

/// The envelope a dispatch runs under: its sequence number, the inbound envelope it is the handling of (none for
/// an external event) and how many deliveries earlier units of the dispatch committed.
#[derive(Debug, Default)]
pub(crate) struct Origin {
    pub(super) seq: u64,
    pub(super) inbound: Option<Inbound>,
    pub(super) emits: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct Inbound {
    pub(super) causal_id: String,
    pub(super) hop: u32,
}

#[derive(Default)]
pub(crate) struct Deliveries {
    deployment: HashMap<String, String>,
    seen: HashMap<String, HashSet<String>>,
    seq: u64,
    held: Option<Origin>,
}

impl Deliveries {
    /// The program's unit placement, which a fresh session of the same program keeps; what was seen and sent is the
    /// session's own.
    pub(crate) fn copy_placement(&mut self, from: &Deliveries) {
        self.deployment = from.deployment.clone();
    }
}

/// An inbound envelope that was admitted: the traits to deliver it to, and the origin its dispatch carries.
pub(super) struct Admitted {
    pub(super) targets: Vec<String>,
    pub(super) inbound: Inbound,
}

/// `source_payload` through a listener's `with` mapping: the payload the listener's trigger is delivered with.
pub(crate) fn mapped_listen_payload(
    evaluator: &Evaluator,
    ctx: &BindingContext,
    mapping: &Option<SObject>,
    source_payload: &Value,
) -> Value {
    match mapping {
        Some(mapping) => {
            let mut mapped = ValueMap::default();
            for (key, expr) in mapping {
                if let Ok(value) = evaluator.evaluate(expr, ctx) {
                    mapped.insert(key.clone(), value);
                }
            }
            Value::object(mapped)
        }
        None => source_payload.clone(),
    }
}

impl RuntimeKernel {
    pub(super) fn register_placement(&self, schema: &OrbitalSchema) {
        let Some(deployment) = &schema.deployment else {
            return;
        };
        let mut deliveries = self.deliveries.write();
        for (unit, orbitals) in deployment {
            for orbital in orbitals {
                deliveries.deployment.insert(orbital.clone(), unit.clone());
            }
        }
    }

    /// Whether `orbital` is placed in a unit other than this kernel's. False without a configured unit or a
    /// placement of `orbital`: a program with no `deployment` keeps every delivery local.
    pub(super) fn is_remote(&self, orbital: &str) -> bool {
        let Some(unit) = self.config.unit.as_deref() else {
            return false;
        };
        self.deliveries
            .read()
            .deployment
            .get(orbital)
            .is_some_and(|placed| placed != unit)
    }

    /// `listeners` without those placed in another unit: those are reached by an envelope, never by the worklist.
    pub(super) fn local_only(&self, listeners: Vec<ListenerInfo>) -> Vec<ListenerInfo> {
        if self.config.unit.is_none() {
            return listeners;
        }
        listeners
            .into_iter()
            .filter(|listener| !self.is_remote(&listener.orbital))
            .collect()
    }

    pub(super) fn next_dispatch_origin(&self) -> Origin {
        let mut deliveries = self.deliveries.write();
        deliveries.seq += 1;
        Origin {
            seq: deliveries.seq,
            inbound: None,
            emits: 0,
        }
    }

    pub(super) fn hold_origin(&self, origin: &mut Origin) {
        self.deliveries.write().held = Some(std::mem::take(origin));
    }

    /// Give the held origin back to its run. A unit that did not commit hands back the delivery count it began with.
    pub(super) fn release_origin(&self, origin: &mut Origin, committed_from: u32, committed: bool) {
        let mut held = self.deliveries.write().held.take().unwrap_or_default();
        if !committed {
            held.emits = committed_from;
        }
        *origin = held;
    }

    /// Ask the host to deliver each `emit` of the step whose declared listener is placed in another unit, in
    /// result order then listener registration order. A refusal is recorded on the emit's result as a
    /// [`DeliveryRefused`]; the returned message is the step's failure when nothing the emitter's listens would
    /// hear the `DELIVERY_REJECTED`. `None` also when the step is suspended on an unanswered ask.
    pub(super) fn deliver_remote_emits(
        &self,
        trait_name: &str,
        follow_on: bool,
        results: &mut [EffectResult],
    ) -> Option<String> {
        let unit = self.config.unit.as_deref()?;
        let (seq, base, inbound) = {
            let deliveries = self.deliveries.read();
            if deliveries.deployment.is_empty() {
                return None;
            }
            let held = deliveries.held.as_ref()?;
            (held.seq, held.emits, held.inbound.clone())
        };
        let source = self.emit_source(trait_name);
        let from_orbital = source.orbital.clone()?;
        let hop = inbound.as_ref().map_or(0, |inbound| inbound.hop) + 1;
        let mut causal_id = inbound.map(|inbound| inbound.causal_id);
        let mut ordinal = base;
        let mut refusals: Vec<(usize, DeliveryRefused)> = Vec::new();
        for (index, result) in results.iter().enumerate() {
            let Effect::Emit { event, payload } = &result.effect else {
                continue;
            };
            let remote: Vec<ListenerInfo> = self
                .listener_manager
                .matching(event, &source)
                .into_iter()
                .filter(|listener| self.is_remote(&listener.orbital))
                .collect();
            if remote.is_empty() {
                continue;
            }
            let evaluator = Evaluator::new();
            let ctx = BindingContextBuilder::new()
                .payload(payload.clone())
                .build();
            for listener in remote {
                let Some(to_unit) = self
                    .deliveries
                    .read()
                    .deployment
                    .get(&listener.orbital)
                    .cloned()
                else {
                    continue;
                };
                let causal = causal_id
                    .get_or_insert_with(|| format!("{unit}#{seq}"))
                    .clone();
                let envelope = Envelope {
                    id: envelope_id(unit, &causal, seq, ordinal),
                    from: InstanceRef {
                        instance: unit.to_string(),
                        orbital: from_orbital.clone(),
                        trait_name: Some(trait_name.to_string()),
                    },
                    to: InstanceRef {
                        instance: to_unit,
                        orbital: listener.orbital.clone(),
                        trait_name: Some(listener.trait_name.clone()),
                    },
                    event: listener.triggers.clone(),
                    payload: mapped_listen_payload(
                        &evaluator,
                        &ctx,
                        &listener.payload_mapping,
                        payload,
                    ),
                    causal_id: causal,
                    hop,
                };
                ordinal += 1;
                let refusal = if hop > HOP_CAP {
                    Some(DeliverRefusal {
                        kind: DeliverRefusalKind::HopCap,
                        id: envelope.id.clone(),
                    })
                } else {
                    let site = CallSite {
                        orbital: Some(from_orbital.clone()),
                        trait_name: trait_name.to_string(),
                        follow_on,
                        entity_id: crate::process_result::RUNTIME_SINGLETON.to_string(),
                        emit: CallRoutes::default(),
                    };
                    match self.io.deliver(&envelope, site) {
                        DeliverAsk::Suspended => return None,
                        DeliverAsk::Answered(Ok(Accepted)) => None,
                        DeliverAsk::Answered(Err(refusal)) => Some(refusal),
                    }
                };
                if let Some(refusal) = refusal {
                    refusals.push((
                        index,
                        DeliveryRefused {
                            id: refusal.id,
                            to: envelope.to,
                            event: envelope.event,
                            reason: refusal.kind,
                            causal_id: envelope.causal_id,
                        },
                    ));
                }
            }
        }
        if let Some(held) = self.deliveries.write().held.as_mut() {
            held.emits = ordinal;
        }
        if refusals.is_empty() {
            return None;
        }
        let heard = !self
            .local_only(self.listener_manager.matching(DELIVERY_REJECTED, &source))
            .is_empty();
        let failure = (!heard).then(|| {
            refusals
                .iter()
                .map(|(_, refused)| {
                    format!(
                        "delivery of '{}' to {}/{} refused ({}, envelope {}); no trait listens for {DELIVERY_REJECTED}",
                        refused.event,
                        refused.to.instance,
                        refused.to.orbital,
                        refused.reason.as_str(),
                        refused.id,
                    )
                })
                .collect::<Vec<_>>()
                .join("; ")
        });
        for (index, refused) in refusals {
            results[index].refused.push(refused);
        }
        failure
    }

    /// The `DELIVERY_REJECTED` each refused delivery of `result` queues for the emitter's declared listeners.
    pub(super) fn refusal_followups(
        &self,
        trait_name: &str,
        source: &BusEventSource,
        result: &ProcessEventResult,
        fanned: &mut Vec<CascadeItem>,
    ) {
        for emit in &result.effect_results {
            for refused in &emit.refused {
                self.collect_listener_targets(
                    source,
                    trait_name,
                    DELIVERY_REJECTED,
                    &refused.payload(),
                    &[],
                    fanned,
                );
            }
        }
    }

    fn orbital_traits(&self, orbital: &str) -> Option<Vec<String>> {
        self.registry.schemas.read().values().find_map(|schema| {
            schema.orbitals.iter().find(|o| o.name == orbital).map(|o| {
                o.traits
                    .iter()
                    .filter_map(|reference| reference.trait_name().map(str::to_string))
                    .collect()
            })
        })
    }

    /// Judge an inbound envelope: `hop-cap` past [`HOP_CAP`]; `unknown-instance` when its orbital (or trait) is not
    /// declared here or not placed in this kernel's unit; `Duplicate` for an id already accepted from the sender.
    /// An accepted id is journaled before the dispatch runs.
    pub(super) fn admit_envelope(&self, envelope: &Envelope) -> Result<Admitted, EnvelopeVerdict> {
        let refuse = |kind| {
            EnvelopeVerdict::Rejected(DeliverRefusal {
                kind,
                id: envelope.id.clone(),
            })
        };
        if envelope.hop > HOP_CAP {
            return Err(refuse(DeliverRefusalKind::HopCap));
        }
        let here = self
            .config
            .unit
            .as_deref()
            .is_none_or(|unit| unit == envelope.to.instance)
            && !self.is_remote(&envelope.to.orbital);
        let Some(traits) = self.orbital_traits(&envelope.to.orbital).filter(|_| here) else {
            return Err(refuse(DeliverRefusalKind::UnknownInstance));
        };
        let targets = match &envelope.to.trait_name {
            Some(target) if traits.contains(target) => vec![target.clone()],
            Some(_) => return Err(refuse(DeliverRefusalKind::UnknownInstance)),
            None => self.discover_handlers(&traits, None, &envelope.event),
        };
        if let Some(refusal) =
            self.payload_rejection(targets.iter().map(|t| (t.as_str(), envelope.event.as_str())), &envelope.payload)
        {
            tracing::warn!(target: orbital_core_base::logging::ns::RUNTIME_KERNEL, id = %envelope.id, %refusal, "envelope:payload-invalid");
            return Err(refuse(DeliverRefusalKind::PayloadInvalid));
        }
        let first = self
            .deliveries
            .write()
            .seen
            .entry(envelope.from.instance.clone())
            .or_default()
            .insert(envelope.id.clone());
        if !first {
            return Err(EnvelopeVerdict::Duplicate);
        }
        Ok(Admitted {
            targets,
            inbound: Inbound {
                causal_id: envelope.causal_id.clone(),
                hop: envelope.hop,
            },
        })
    }
}
