use std::process::{Command, Stdio};
use std::io::{BufReader, BufRead};
use lsp_types::{Range, Position};

use crate::Error;

type DirectoryPath = String;
type PatternString = String;
type EngineName = String;

#[derive(Debug)]
pub struct Match {
    pub filename: String,
    pub range: Range,
    pub before: String,
    pub matched: String,
    pub after: String,
    pub pattern: PatternString,
}

impl Match {
    fn new(pattern: PatternString, match_str: String) -> Match {
        let terms: Vec<&str> = match_str.split('\n').collect();

        if let [_, filename, line_number, before, matched, after] = terms.as_slice() {
            // Need to remove one, because we count line numbers from 0
            let line_number = line_number.parse::<u64>().unwrap() - 1;
            Match {
                filename: filename.to_string(),
                range: Range {
                    start: Position {
                        line: line_number,
                        character: before.len() as u64,
                    },
                    end: Position {
                        line: line_number,
                        character: (before.len() + matched.len()) as u64,
                    }
                },
                before: before.to_string(),
                matched: matched.to_string(),
                after: after.to_string(),
                pattern: pattern,
            }
        } else {
            panic!("Unexpected match format");
        }
    }
}

pub trait Search : Send {
    fn search(&self, pattern_string: PatternString) -> Result<Vec<Match>, Error>;
}

struct SearchAck {
    directory_path: DirectoryPath,
    languages: Vec<String>,
    exec_path: String,
}

impl SearchAck {
    fn new(launcher: SearchLauncher) -> Result<Box<dyn Search>, Error> {
        Ok(Box::new(SearchAck {
            directory_path: launcher.directory,
            languages: launcher.languages,
            exec_path: "/usr/bin/ack".to_owned()
        }))
    }

    fn compose_args(&self, pattern_string: PatternString) -> Vec<String> {
        let mut args = Vec::new();
        for lang in self.languages.clone() {
            args.push("--type".to_owned());
            args.push(lang);
        }

        // Make the output of awk easy to parse
        args.push("--output".to_owned());
        args.push("\n$f\n$.\n$`\n$&\n$'".to_owned());

        // separate entries with zeroes
        args.push("--print0".to_owned());

        args.push(pattern_string);
        args
    }
}

impl Search for SearchAck {
    fn search(&self, pattern_string: PatternString) -> Result<Vec<Match>, Error> {
        let mut cmd = Command::new(self.exec_path.as_str())
            .args(self.compose_args(pattern_string.clone()))
            .current_dir(self.directory_path.as_str())
            .stdout(Stdio::piped())
            .spawn()?;

        let mut stdout = cmd.stdout.as_mut().expect("Failed to get stdout");

        let mut matches = Vec::new();
        let mut reader = BufReader::new(&mut stdout);
        loop {
            let mut buffer = Vec::new();
            match reader.read_until(0, &mut buffer) {
                Ok(0) => {
                    break;
                }
                Ok(_) => {
                    let single_match = String::from_utf8(buffer[..buffer.len() - 1].to_vec())?;
                    matches.push(Match::new(pattern_string.clone(), single_match));
                }
                Err(e) => {
                    return Err(Box::new(e));
                }
            }
        }

        Ok(matches)
    }
}

pub struct SearchLauncher {
    engine: EngineName,
    directory: DirectoryPath,
    languages: Vec<String>,
}

impl SearchLauncher {
    pub fn new() -> SearchLauncher {
        SearchLauncher {
            engine: "ack".to_owned(),
            directory: "".to_owned(),
            languages: vec!["".to_owned()],
        }
    }

    pub fn directory(mut self, path: &str) -> SearchLauncher {
        self.directory = path.to_string();
        self
    }

    pub fn engine(mut self, engine: &str) -> SearchLauncher {
        self.engine = engine.to_string();
        self
    }

    pub fn languages(mut self, languages: &Vec<String>) -> SearchLauncher {
        self.languages = languages.clone();
        self
    }

    pub fn launch(self) -> Result<Box<dyn Search>, Error> {
        match self.engine.as_str() {
            "ack" => SearchAck::new(self),
            _ => panic!("Unknown engine"),
        }
    }
}
