use anyhow::Result;
use serde::Deserialize;
use std::fs;

pub trait FileList {
    fn iter(&self) -> std::slice::Iter<String>;
    fn len(&self) -> usize;
}

pub struct CompileCommands {
    files: Vec<String>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct File {
    arguments: Vec<String>,
    directory: String,
    file: String,
    output: Option<String>,
}

impl CompileCommands {
    pub fn new(compile_commands_file: &str) -> Result<Self> {
        let file = fs::File::open(compile_commands_file)?;
        let json: Vec<File> = serde_json::from_reader(file)?;
        Ok(Self {
            files: json.into_iter().map(|f| f.file).collect(),
        })
    }
}

impl FileList for CompileCommands {
    fn iter(&self) -> std::slice::Iter<String> {
        self.files.iter()
    }

    fn len(&self) -> usize {
        self.files.len()
    }
}
