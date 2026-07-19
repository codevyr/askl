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

pub struct ExecutionContext {
    pub registry: SelectorRegistry,
    pub current_statement_span: Option<Span>,
    /// Ephemeral visibility chain for the current request.
    pub eph: EphContext,
    /// Every eph-layer touch made while executing this request, in statement
    /// order.  Lets callers (tests, diagnostics) observe whether each layer
    /// was freshly populated or served from cache.
    pub layer_activations: Vec<crate::command::LayerActivation>,
}

impl ExecutionContext {
    pub fn new() -> Self {
        Self {
            registry: SelectorRegistry::new(),
            current_statement_span: None,
            eph: EphContext::new(),
            layer_activations: Vec::new(),
        }
    }
}
