use juniper::{EmptyMutation, RootNode};

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

#[juniper::object]
impl QueryRoot {
    fn f() -> Vec<Function> {
        vec![
            Function {
                name: "foo".to_owned(),
            },
            Function {
                name: "bar".to_owned(),
            },
        ]
    }
}

pub type Schema = RootNode<'static, QueryRoot, EmptyMutation<()>>;

pub fn create_schema() -> Schema {
    Schema::new(QueryRoot {}, EmptyMutation::new())
}
