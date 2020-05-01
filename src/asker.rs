use log::{info};

use std::collections::HashMap;

use lsp_types::{DocumentSymbolResponse, SymbolKind, TextDocumentItem, Range, DocumentSymbol};

use crate::{Opt, Error, LspError};

use crate::language_server::{LanguageServerLauncher, LanguageServer};

use crate::search::{SearchLauncher, Search, Match};

use crate::schema::Symbol;

fn parse_list(src: &str) -> Vec<String> {
    src.split(',').map(str::to_string).collect()
}

#[derive(Debug, Clone)]
pub struct AskerSymbol {
    pub name: String,
    range: Range,
    kind: SymbolKind,
    parent: Option<usize>,
}

struct AskerDocument {
    symbols: Vec<AskerSymbol>,
    lsp_item: TextDocumentItem,
}

impl AskerDocument {
    fn new(document: TextDocumentItem) -> Self {
        AskerDocument {
            lsp_item: document,
            symbols: Vec::new(),
        }
    }

    fn append_symbol(&mut self, symbol: &DocumentSymbol, parent: Option<usize>) -> Result<(), Error> {
        self.symbols.push(AskerSymbol{
            parent: parent,
            kind: symbol.kind.clone(),
            name: symbol.name.clone(),
            range: symbol.range.clone(),
        });

        let current_id = self.symbols.len() - 1;
        if let Some(children) = &symbol.children {
            for child in children {
                self.append_symbol(&child, Some(current_id))?;
            }
        }

        Ok(())
    }
}

/// Structure that maintains metadata for the commands to run
pub struct Asker {
    documents: HashMap<String, AskerDocument>,
    lang_server: Box<dyn LanguageServer>,
    searcher: Box<dyn Search>,
}

impl Asker {
    pub fn new(opt: &Opt) -> Result<Asker, Error> {
        let language_list = parse_list(&opt.languages);

        let searcher = SearchLauncher::new()
            .engine("ack")
            .directory(&opt.project_root)
            .languages(&language_list)
            .launch()?;

        let mut lang_server = LanguageServerLauncher::new()
            .server("/usr/bin/clangd-9".to_owned())
            .project(opt.project_root.to_owned())
            .languages(language_list)
            .launch()
            .expect("Failed to spawn clangd");

        lang_server.initialize()?;
        lang_server.initialized()?;

        Ok(Asker {
            lang_server: lang_server,
            searcher: searcher,
            documents: HashMap::new(),
        })
    }

    fn update_symbols(&mut self, document: &mut AskerDocument) -> Result<(), Error> {
        let symbols = self.lang_server.document_symbol(&document.lsp_item)?;
        match symbols {
            Some(DocumentSymbolResponse::Flat(_)) => {
                Err(Box::new(LspError("Flat symbols are unsupported")))
            },
            Some(DocumentSymbolResponse::Nested(v)) => {
                for symbol in v.iter() {
                    document.append_symbol(symbol, None)?;
                }
                Ok(())
            },
            None => {
                Err(Box::new(LspError("No symbols found")))
            }
        }
    }

    fn update_documents(&mut self, matches: &Vec<Match>) -> Result<(), Error> {
        for m in matches {
            if let Some(_) = self.documents.get(&m.filename) {
                continue
            }

            let mut document = AskerDocument::new(self.lang_server.document_open(m.filename.as_str())?);

            self.update_symbols(&mut document)?;
            self.documents.insert(m.filename.clone(), document);
        }

        Ok(())
    }

    pub fn search(&mut self, pattern_string: &str) -> Result<Vec<Match>, Error> {
        let matches = self.searcher.search(pattern_string.to_owned())?;

        self.update_documents(&matches)?;

        Ok(matches)
    }

    pub fn find_symbols(&mut self, matches: &Vec<Match>) -> Vec<Symbol> {
        matches
            .iter()
            .map(|search_match| {
                let document = self.documents.get(&search_match.filename).unwrap();
                let symbol = document.symbols
                    .iter()
                    .rev()
                    .skip_while(|s| s.range.start.line > search_match.line_number)
                    .nth(0);
                info!("Symbol: {:#?} Search: {:#?}", symbol, search_match);
                if let Some(symbol) = symbol {
                    if symbol.range.start.line == search_match.line_number {
                        return Some(Symbol {
                            name: symbol.name.clone(),
                        });
                    }
                }
                None
            })
            .filter_map(|s| s)
            .collect()
    }

    pub fn find_parent(&mut self, search_match: Match) -> Option<Symbol> {
        let document = self.documents.get(&search_match.filename).unwrap();

        let symbol = document.symbols.iter().rev().skip_while(|s| s.range.start.line > search_match.line_number).nth(0);

        match symbol {
            Some(symbol) => {
                if symbol.range.start.line == search_match.line_number {
                    // Found oneself
                    None
                } else {
                    Some(Symbol {
                        name: symbol.name.clone(),
                    })
                }
            },
            None => None,
        }
    }
}

impl Drop for Asker {
    fn drop(&mut self) {
        self.lang_server.shutdown().expect("Shutdown message failed");
        self.lang_server.exit().expect("Exit failed");
    }
}
