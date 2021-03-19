#[macro_use]
extern crate pest;
#[macro_use]
extern crate pest_derive;

#[derive(Parser)]
#[grammar = "askl.pest"]
pub struct AsklParser;

#[cfg(test)]
mod tests {
    use super::*;
    use pest::parses_to;

    use crate::AsklParser;

    #[test]
    fn all() {
        parses_to! {
            parser: AsklParser,
            input: "{}",
            rule: Rule::ask,
            tokens: [ask(0, 2, [
                operation(0, 2, [
                    filter(0, 2)
                ])
            ])]
        };
    }

    #[test]
    fn empty() {
        parses_to! {
            parser: AsklParser,
            input: "",
            rule: Rule::ask,
            tokens: [ask(0, 0)]
        };
    }
}
