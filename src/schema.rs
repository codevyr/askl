use juniper::{EmptyMutation, RootNode};
use std::sync::{Arc, Mutex};

use crate::asker::Asker;

pub struct Context {
    pub asker: Arc<Mutex<Asker>>,
}

impl juniper::Context for Context {}

#[derive(Debug)]
pub struct Symbol {
    pub name: String,
}

#[juniper::object(
    Context = Context,
    description = "Symbol query"
)]
impl Symbol {
    fn name(&self) -> &str {
        self.name.as_str()
    }

    fn parents(&self, context: &Context) -> Vec<Symbol> {
        println!("Looking for: {:?}", self.name);
        let mut asker = context.asker.lock().unwrap();
        println!("  Looking for: {:?}", self.name);
        let matches = asker.search(self.name.as_str());
        println!("    Looking for: {:?}", self.name);

        if let Err(_) = matches {
            return vec![];
        }

        let mut parents = Vec::new();
        for m in matches.unwrap() {
            println!("      Looking for: {:?} {:?}", self.name, m);
            if let Some(s) = asker.find_parent(m) {
                parents.push(Symbol {
                    name: s.name,
                });
            }
        }

        parents.sort_by(|a, b| a.name.cmp(&b.name));
        parents.dedup_by(|a, b| a.name == b.name);

        parents
    }
}

pub struct QueryRoot;

#[juniper::object(
    Context = Context,
)]
impl QueryRoot {
    fn f(context: &Context, name: Option<String>) -> Vec<Symbol> {
        match name {
            Some(name) => {
                let mut asker = context.asker.lock().unwrap();
                let matches = asker.search(name.as_str()).unwrap();
                let mut children = asker.find_symbols(&matches);

                children.iter().map(|s| Symbol {
                    name: s.name.clone(),
                }).collect()
            },
            None => Vec::new()
        }
    }

    fn cfg(context: &Context) -> Option<Symbol> {
        None
    }

    fn grand_parents(&self, context: &Context, name: String) -> &QueryRoot {
        self
    }
}

pub type Schema = RootNode<'static, QueryRoot, EmptyMutation<Context>>;

pub fn create_schema() -> Schema {
    Schema::new(QueryRoot {}, EmptyMutation::new())
}

