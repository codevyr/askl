use std::{collections::HashMap, fmt};
use pest::{error::ErrorVariant, iterators::{Pair, Pairs}};

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
    params: Vec<Pair<'i, Rule>>,
    find: Vec<Pair<'i, Rule>>,
    command: Pair<'i, Rule>,
    // pairs: Pairs<'i, Rule>
}

#[derive(Debug)]
pub struct AskError {
    pest_err: pest::error::Error<Rule>
}

impl AskError {
    fn new(message: &str, span: pest::Span) -> AskError {
        AskError{
            pest_err: pest::error::Error::new_from_span(
                ErrorVariant::CustomError{message: message.to_string()}, span)
        }
    }
}
type Result<T> = std::result::Result<T, AskError>;

impl fmt::Display for AskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AskError: {}", self.pest_err)
    }
}

impl<'i> Askl<'i> {
    fn extract(pairs: Pairs<'i, Rule>) -> Result<Askl> {
        let mut params = Vec::<Pair<'i, Rule>>::new();
        let mut find = Vec::<Pair<'i, Rule>>::new();
        let mut command = Vec::<Pair::<'i, Rule>>::new();
        pairs.for_each(|pair| {
            println!("{:#?}", pair);
            match pair.as_rule() {
                Rule::param => {
                    params.push(pair);
                },
                Rule::find => {
                    find.push(pair)
                },
                Rule::operation => {
                    command.push(pair)
                }
                Rule::EOI => (),
                _ => unreachable!(),
            };
        });

        if command.len() > 1 {
            let first_command = command.first().unwrap().as_span();
            let (l, c) = first_command.start_pos().line_col();
            println!("More than one outer command defined. Previous defined at line {} column {}: \n{}",
                     l, c, first_command.start_pos().line_of());
            return Err(AskError::new("Operation redefined", first_command));
        }
        Ok(Askl{params, find, command:command.pop().unwrap()})
    }

    pub fn new(ask_code: &str) -> Result<Askl> {
        println!("Parsing: {}", ask_code);
        match AsklParser::parse(Rule::ask, ask_code) {
            Ok(pairs) => {
                println!("{:#?}", pairs);

                let askl = Askl::extract(pairs);
                askl
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
