use std::collections::HashMap;

use index::db_diesel::{EphContext, Selection};

use crate::span::Span;
use crate::verb::{Selector, SelectorId, SelectorState};

pub struct SelectorRegistry(HashMap<SelectorId, SelectorState>);

impl SelectorRegistry {
    fn new() -> Self {
        Self(HashMap::new())
    }

    pub fn add(&mut self, selector: &dyn Selector, selection: Option<Selection>) {
        self.add_by_id(selector.id(), selection);
    }

    pub fn add_by_id(&mut self, id: SelectorId, selection: Option<Selection>) {
        self.0.insert(id, SelectorState { selection });
    }

    pub fn contains(&self, id: &SelectorId) -> bool {
        self.0.contains_key(id)
    }

    pub fn for_each_selector_mut<'a, S, F>(&mut self, selectors: S, mut f: F)
    where
        S: 'a + Iterator<Item = &'a dyn Selector>,
        F: FnMut(&dyn Selector, &mut SelectorState),
    {
        for selector in selectors {
            let entry = self
                .0
                .get_mut(&selector.id())
                .expect("Selector should have been added");
            f(selector, entry);
        }
    }

    pub fn for_each_selector<'a, S, F>(&self, selectors: S, mut f: F)
    where
        S: 'a + Iterator<Item = &'a dyn Selector>,
        F: FnMut(&dyn Selector, &SelectorState),
    {
        for selector in selectors {
            let entry = self
                .0
                .get(&selector.id())
                .expect("Selector should have been added");
            f(selector, entry);
        }
    }
}

pub fn selector_state_with<R, S>(
    registry: &mut SelectorRegistry,
    selector: &S,
    f: impl FnOnce(&mut SelectorState) -> R,
) -> R
where
    S: Selector + ?Sized,
{
    let entry = registry
        .0
        .entry(selector.id())
        .or_insert_with(SelectorState::new);
    f(entry)
}

/// Default cardinality-probe cap: a probe fetching at most this many
/// instance ids resolves the statement exactly; more means "capped" and
/// the statement keeps its predicate-driven read.  Overridden per server
/// via `--probe-cap`.
pub const DEFAULT_PROBE_CAP: usize = 1000;

/// One cardinality probe made while executing a request — the probe-phase
/// counterpart of [`crate::command::LayerActivation`], recorded so tests
/// and diagnostics can observe which statements probed and how they
/// classified.
#[derive(Debug, Clone)]
pub struct ProbeActivation {
    /// The probed statement's source text.
    pub query_statement: String,
    /// `Some(n)` — resolved with exactly `n` instance ids (the emission
    /// switched to a by-id fetch); `None` — capped, predicate path kept.
    pub resolved: Option<usize>,
    /// Which probe wave produced this: `0` = the unconstrained wave-0
    /// probe, `n > 0` = the n-th semi-join refinement round (the probe ran
    /// under roles from neighbours resolved in earlier rounds).
    pub round: usize,
}

pub struct ExecutionContext {
    pub registry: SelectorRegistry,
    pub current_statement_span: Option<Span>,
    /// Ephemeral visibility chain for the current request.
    pub eph: EphContext,
    /// Every eph-layer touch made while executing this request, in statement
    /// order.  Lets callers (tests, diagnostics) observe whether each layer
    /// was freshly populated or served from cache.
    pub layer_activations: Vec<crate::command::LayerActivation>,
    /// Cardinality-probe cap for this request (see [`DEFAULT_PROBE_CAP`]).
    pub probe_cap: usize,
    /// Every cardinality probe made while executing this request, in
    /// statement order.
    pub probe_activations: Vec<ProbeActivation>,
    /// Probe-resolved statements: statement identity (its `Rc` pointer as
    /// `usize`) → the exact instance-id set its probe resolved to.
    /// Written by the probe phase (wave 0 and refinement rounds), read by
    /// the Phase-R emission (by-id fetch) and by the scope builders (a
    /// resolved neighbour contributes its ids instead of a condition the
    /// index would have to materialise).  Refinement-resolved sets are
    /// exact only *in composition* (predicate ∧ neighbour roles) — always a
    /// superset of the statement's final selection, which is what both
    /// consumers require.
    pub probe_resolved: HashMap<usize, Vec<i64>>,
}

/// Key for [`ExecutionContext::probe_resolved`]: statement identity by
/// `Rc` pointer.
pub fn statement_key<T>(statement: &std::rc::Rc<T>) -> usize {
    std::rc::Rc::as_ptr(statement) as usize
}

impl ExecutionContext {
    /// A context is always rooted in an explicit root-layer set (resolved
    /// per request via `Index::load_root_layers`) — visibility of persistent
    /// data is an allowlist, never ambient.
    pub fn new(roots: Vec<index::db_diesel::RootLayer>) -> Self {
        Self {
            registry: SelectorRegistry::new(),
            current_statement_span: None,
            eph: EphContext::rooted(roots),
            layer_activations: Vec::new(),
            probe_cap: DEFAULT_PROBE_CAP,
            probe_activations: Vec::new(),
            probe_resolved: HashMap::new(),
        }
    }
}
