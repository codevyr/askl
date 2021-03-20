use std::{collections::HashMap, fmt};
use pest::iterators::Pairs;

#[macro_use]
extern crate pest;
#[macro_use]
extern crate pest_derive;

use pest::Parser;

#[derive(Parser)]
#[grammar = "askl.pest"]
struct AsklParser;

type Params<'i> = HashMap<&'i str, Rule>;

pub struct Askl<'i> {
    params: Params<'i>,
    pairs: Pairs<'i, Rule>
}

#[derive(Debug)]
pub struct AskError {
    pest_err: pest::error::Error<Rule>
}

impl fmt::Display for AskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AskError: {}", self.pest_err)
    }
}

impl<'i> Askl<'i> {
    fn extract_params(pairs: Pairs<'i, Rule>) -> (Pairs<'i, Rule>, Params) {
        (pairs, HashMap::new())
    }

    pub fn new(ask_code: &str) -> Result<Askl, AskError> {
        println!("Parsing: {}", ask_code);
        match AsklParser::parse(Rule::ask, ask_code) {
            Ok(mut pairs) => {
                println!("{:#?}", pairs);

                let (pairs, params) = Askl::extract_params(pairs);
                Ok(Askl{params, pairs})
            },
            Err(error) => {
                println!("Error: {}", error);
                Err(AskError{pest_err: error})
            }
        }
    }
}


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
            tokens: [
                operation(0, 2, [
                    filter(0, 2, [filter_params(0, 0)])
                ]),
            ]
        };
    }

    #[test]
    fn empty() {
        parses_to! {
            parser: AsklParser,
            input: "",
            rule: Rule::ask,
            tokens: []
        };
    }

    #[test]
    fn simple1() {
        parses_to! {
            parser: AsklParser,
            input: "\"ib_vesd\" {}",
            rule: Rule::ask,
            tokens: [
                operation(0, 12, [
                    filter(0, 12, [
                        filter_params(0, 9)
                    ])
                ])
            ]
        };
    }
}
