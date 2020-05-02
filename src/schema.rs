use juniper::{EmptyMutation, RootNode, FieldResult, FieldError};
use std::sync::{Arc, Mutex};

use lsp_types;

use crate::asker::Asker;
use crate::search;

pub struct Context {
    pub asker: Arc<Mutex<Asker>>,
}

impl juniper::Context for Context {}

#[derive(Debug)]
pub struct Symbol {
    pub name: String,
    pub filename: String,
    pub range: Range,
}

#[derive(Debug, Clone)]
pub struct Position {
    lsp: lsp_types::Position,
}

#[juniper::object(description = "Position in the file")]
impl Position {
    fn line(&self) -> i32 {
        self.lsp.line as i32 + 1
    }

    fn character(&self) -> i32 {
        self.lsp.character as i32 + 1
    }
}

impl From<lsp_types::Position> for Position {
    fn from(lsp: lsp_types::Position) -> Position {
        Position {
            lsp: lsp,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Range {
    pub lsp: lsp_types::Range,
}

impl From<lsp_types::Range> for Range {
    fn from(lsp: lsp_types::Range) -> Range {
        Range {
            lsp: lsp,
        }
    }
}

#[juniper::object(description = "Range of the symbol in the file")]
impl Range {
    fn start(&self) -> Position {
        Position::from(self.lsp.start)
    }

    fn end(&self) -> Position {
        Position::from(self.lsp.end)
    }
}

type Match = search::Match;

#[juniper::object(
    Context = Context,
    description = "Search match"
)]
impl Match {
    fn pattern(&self) -> &str {
        self.pattern.as_str()
    }

    fn filename(&self) -> &str {
        self.filename.as_str()
    }

    fn range(&self) -> Range {
        Range::from(self.range)
    }
}

#[juniper::object(
    Context = Context,
    description = "Symbol query"
)]
impl Symbol {
    fn name(&self) -> &str {
        self.name.as_str()
    }

    fn filename(&self) -> &str {
        self.filename.as_str()
    }

    fn range(&self) -> &Range {
        &self.range
    }

    fn parents(&self, context: &Context, name_filter: Option<String>) -> Vec<Symbol> {
        let mut asker = context.asker.lock().unwrap();
        let matches = asker.search(self.name.as_str());

        if let Err(_) = matches {
            return vec![];
        }

        let mut parents = Vec::new();
        for m in matches.unwrap() {
            if let Some(s) = asker.find_parent(m) {
                if name_filter.is_some() && name_filter.as_ref().unwrap().ne(&s.name) {
                    continue;
                }
                parents.push(s);
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
    fn s(context: &Context, name: Option<String>) -> Vec<Symbol> {
        match name {
            Some(name) => {
                let mut asker = context.asker.lock().unwrap();
                let matches = asker.search(name.as_str()).unwrap();
                let mut children = asker.find_symbols(&matches);

                children
            },
            None => Vec::new()
        }
    }

    #[graphql(name="match")]
    fn matches(context: &Context, pattern: String) -> FieldResult<Vec<Match>> {
        let mut asker = context.asker.lock().unwrap();
        asker
            .search(pattern.as_str())
            .map_err(FieldError::from)
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

