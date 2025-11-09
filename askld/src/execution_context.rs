use std::collections::HashMap;

use index::db_diesel::Selection;

use crate::verb::{Selector, SelectorId, SelectorState};

pub struct SelectorRegistry(HashMap<SelectorId, SelectorState>);

impl SelectorRegistry {
    fn new() -> Self {
        Self(HashMap::new())
    }

    pub fn add(&mut self, selector: &dyn Selector, selection: Option<Selection>) {
        self.0.insert(selector.id(), SelectorState { selection });
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
}

impl ExecutionContext {
    pub fn new() -> Self {
        Self {
            registry: SelectorRegistry::new(),
        }
    }
}
