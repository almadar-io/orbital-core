//! What one render slot's `array/map` calls made last time, kept by the element each result was made from
//! (EXEC-RENDER-9). The render store owns one per slot and hands it to the evaluation through the context; a
//! map then runs its callback only for the elements that changed.
//!
//! A result is kept only for a callback written in place whose every operator is declared `pure` or `readsLocale`
//! in the operator table, and is answered again only for the same element (the same allocation), at the same
//! place when the callback names the index, while every binding the callback reads outside its own parameters
//! holds what it held, and, for a locale reader, under the same locale and catalogs.

use super::context::{BindingContext, LocaleStamp};
use super::operators::array::Callback;
use super::operators::{get_operator, EvalResult};
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_base::sync::Mutex;
use orbital_core_base::value::{Shared, Value};
use std::collections::HashMap;
use std::sync::Arc;

pub type SharedMapMemo = Arc<Mutex<MapMemo>>;

#[derive(Debug, Default)]
pub struct MapMemo {
    render: u64,
    calls: HashMap<usize, Call>,
    sites: HashMap<SiteKey, Site>,
    /// Callbacks applied from a lambda value: their body is not the one read at the site.
    opaque: u64,
    reused: usize,
    computed: usize,
}

/// A callback body's allocation and the parameters it is read under.
type SiteKey = (usize, Vec<Shared<String>>);

#[derive(Debug)]
struct Call {
    /// Held so the allocation the call is keyed by stays this one.
    _source: Shared<Vec<Value>>,
    expression: Arc<SExpression>,
    render: u64,
}

#[derive(Debug)]
struct Site {
    /// Held so the allocation the site is keyed by stays this one.
    _body: SExpression,
    reads: Option<Arc<Reads>>,
    seen: Vec<Option<Value>>,
    locale: Option<LocaleStamp>,
    version: u64,
    kept: HashMap<usize, KeptResult>,
    render: u64,
}

/// What a reusable callback reads besides its parameters.
#[derive(Debug)]
struct Reads {
    tokens: Vec<Shared<String>>,
    locale: bool,
}

#[derive(Debug)]
struct KeptResult {
    /// Held so the allocation the result is keyed by stays this one.
    _element: Value,
    index: usize,
    result: Value,
    render: u64,
}

impl MapMemo {
    /// A render of the slot starts: what the render before the last one made and this one has not asked for goes.
    pub fn begin(&mut self) {
        self.render += 1;
        let render = self.render;
        self.calls.retain(|_, call| call.render + 1 >= render);
        self.sites.retain(|_, site| site.render + 1 >= render);
    }

    /// How many mapped elements were answered from what was kept, and how many were computed.
    pub fn counts(&self) -> (usize, usize) {
        (self.reused, self.computed)
    }

    /// The expression the operator call `source` was read into, when this slot has read it before.
    pub(crate) fn expression(&mut self, source: &Shared<Vec<Value>>) -> Option<Arc<SExpression>> {
        let render = self.render;
        let call = self.calls.get_mut(&(Shared::as_ptr(source) as usize))?;
        call.render = render;
        Some(Arc::clone(&call.expression))
    }

    pub(crate) fn keep_expression(&mut self, source: &Shared<Vec<Value>>, expression: Arc<SExpression>) {
        let call = Call { _source: source.clone(), expression, render: self.render };
        self.calls.insert(Shared::as_ptr(source) as usize, call);
    }

    pub(crate) fn applied_a_lambda_value(&mut self) {
        self.opaque += 1;
    }
}

fn allocation(expr: &SExpression) -> Option<usize> {
    match expr {
        SExpression::Array(items) => Some(Shared::as_ptr(items) as usize),
        SExpression::Object(fields) => Some(Shared::as_ptr(fields) as usize),
        _ => None,
    }
}

fn element_allocation(value: &Value) -> Option<usize> {
    match value {
        Value::Array(items) => Some(Shared::as_ptr(items) as usize),
        Value::Object(fields) => Some(Shared::as_ptr(fields) as usize),
        _ => None,
    }
}

/// What `body` reads besides `params`; `None` when it calls an operator the table does not declare `pure` or
/// `readsLocale`.
fn reads(params: &[&Shared<String>], body: &SExpression) -> Option<Reads> {
    fn walk(expr: &SExpression, params: &[&Shared<String>], reads: &mut Reads) -> Option<()> {
        match expr {
            SExpression::String(text) => {
                if let Some(path) = text.strip_prefix('@') {
                    let root = path.split(['.', '[']).next().unwrap_or(path);
                    if !params.iter().any(|param| param.as_str() == root) && !reads.tokens.contains(text) {
                        reads.tokens.push(text.clone());
                    }
                }
            }
            SExpression::Array(items) => {
                if let Some(SExpression::String(op)) = items.first() {
                    if get_operator(op).is_some() {
                        let declared = orbital_core_base::language::registry::lookup_operator(op).ok()??;
                        if !declared.pure && !declared.reads_locale {
                            return None;
                        }
                        reads.locale |= declared.reads_locale;
                    }
                }
                for item in items.iter() {
                    walk(item, params, reads)?;
                }
            }
            SExpression::Object(fields) => {
                for value in fields.values() {
                    walk(value, params, reads)?;
                }
            }
            SExpression::Null | SExpression::Bool(_) | SExpression::Number(_) => {}
        }
        Some(())
    }
    let mut found = Reads { tokens: Vec::new(), locale: false };
    walk(body, params, &mut found)?;
    Some(found)
}

/// One call's use of what a site kept: each element is answered from it or computed, in the order asked, so an
/// operator that stops early (`array/find`, `array/some`) computes no more than it does without a memo.
pub(crate) struct Kept<'a> {
    memo: &'a SharedMapMemo,
    key: SiteKey,
    version: u64,
    indexed: bool,
    made: Vec<(usize, KeptResult)>,
    reused: usize,
    computed: usize,
}

impl<'a> Kept<'a> {
    /// What `memo` kept for `callback` under the bindings it reads now; `None` when this callback's results are
    /// not kept, so the caller applies it as it does without a memo.
    pub(crate) fn open(memo: &'a SharedMapMemo, callback: &Callback, ctx: &BindingContext) -> Option<Self> {
        let Callback::Literal { params, body } = callback else {
            return None;
        };
        if ctx.holds_linear_local() {
            return None;
        }
        let key: SiteKey = (allocation(body)?, params.iter().map(|param| (*param).clone()).collect());
        let read = {
            let mut memo = memo.lock();
            let render = memo.render;
            let site = memo.sites.entry(key.clone()).or_insert_with(|| Site {
                _body: (*body).clone(),
                reads: reads(params, body).map(Arc::new),
                seen: Vec::new(),
                locale: None,
                version: 0,
                kept: HashMap::new(),
                render,
            });
            Arc::clone(site.reads.as_ref()?)
        };
        let seen: Vec<Option<Value>> = read.tokens.iter().map(|token| ctx.resolve_binding(token)).collect();
        let locale = read.locale.then(|| ctx.locale_stamp());
        let mut held = memo.lock();
        let render = held.render;
        let site = held.sites.get_mut(&key)?;
        let same = site.seen.len() == seen.len()
            && site.seen.iter().zip(&seen).all(|(was, now)| match (was, now) {
                (Some(was), Some(now)) => was.equals_sharing(now),
                (None, None) => true,
                _ => false,
            });
        if !same || site.locale != locale {
            site.kept.clear();
            site.seen = seen;
            site.locale = locale;
            site.version += 1;
        }
        if site.render != render {
            site.kept.retain(|_, kept| kept.render + 1 >= render);
            site.render = render;
        }
        let version = site.version;
        drop(held);
        Some(Kept { memo, key, version, indexed: params.len() >= 2, made: Vec::new(), reused: 0, computed: 0 })
    }

    /// The callback's answer for `item` at `index`.
    pub(crate) fn answer(&mut self, index: usize, item: &Value, callback: &Callback, ctx: &BindingContext) -> EvalResult {
        let at = element_allocation(item);
        let (before, render) = {
            let mut memo = self.memo.lock();
            let render = memo.render;
            let kept = at.and_then(|at| {
                let site = memo.sites.get_mut(&self.key).filter(|site| site.version == self.version)?;
                let kept = site.kept.get_mut(&at)?;
                (!self.indexed || kept.index == index).then(|| {
                    kept.render = render;
                    kept.result.clone()
                })
            });
            if let Some(result) = kept {
                self.reused += 1;
                return Ok(result);
            }
            (memo.opaque, render)
        };
        let result = callback.apply(vec![item.clone(), super::operators::util::int_of(index)], ctx)?;
        self.computed += 1;
        if let (true, Some(at)) = (before == self.memo.lock().opaque, at) {
            self.made.push((at, KeptResult { _element: item.clone(), index, result: result.clone(), render }));
        }
        Ok(result)
    }
}

impl Drop for Kept<'_> {
    fn drop(&mut self) {
        let mut memo = self.memo.lock();
        memo.reused += self.reused;
        memo.computed += self.computed;
        if let Some(site) = memo.sites.get_mut(&self.key).filter(|site| site.version == self.version) {
            site.kept.extend(std::mem::take(&mut self.made));
        }
    }
}
