use juniper::{EmptyMutation, RootNode};
use std::sync::{Arc, Mutex};

use crate::asker::Asker;

pub struct Context {
    pub asker: Arc<Mutex<Asker>>,
}

impl juniper::Context for Context {}

struct Function {
    name: String,
}

#[juniper::object(description = "Function query")]
impl Function {
    pub fn name(&self) -> &str {
        self.name.as_str()
    }
}

pub struct QueryRoot;

#[juniper::object(
    Context = Context,
)]
impl QueryRoot {
    fn f(context: &Context, name: Option<String>) -> Vec<Function> {
        match name {
            Some(name) => {
                vec![
                    Function {
                        name: "foo".to_owned(),
                    },
                    Function {
                        name: "bar".to_owned(),
                    },
                ]
            },
            None => Vec::new()
        }
    }

    fn cfg(context: &Context) -> Option<Function> {
        None
    }

    fn parent(context: &Context, name: String) -> Option<Function> {
        let mut asker = context.asker.lock().unwrap();
        let matches = asker.search(name.as_str());

        if let Err(_) = matches {
            return None
        }

        for m in matches.unwrap() {
            if let Some(s) = asker.find_parent(m) {
                return Some(Function {
                    name: s.name,
                })
            }
        }

        None
    }
}

pub type Schema = RootNode<'static, QueryRoot, EmptyMutation<Context>>;

pub fn create_schema() -> Schema {
    Schema::new(QueryRoot {}, EmptyMutation::new())
}

