use std::collections::HashMap;

use crate::{
    db::{Declaration, Index},
    symbols::{self, FileId, SymbolId, SymbolScope, SymbolType},
};
use anyhow::{anyhow, bail, Result};
use clang_ast::Id;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CompileCommand {
    pub arguments: Option<Vec<String>>,
    pub command: Option<String>,
    pub directory: String,
    pub file: String,
    pub output: Option<String>,
}

pub type Node = clang_ast::Node<Clang>;

fn extract_filter<'a>(root: &'a Node, f: &'a impl Fn(&Node) -> bool) -> Vec<&'a Node> {
    let mut result = vec![];
    if f(root) {
        result.push(root);
    }

    for node in &root.inner {
        result.extend(extract_filter(node, f));
    }

    result
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Clang {
    EnumConstantDecl(EnumConstantDecl),
    // EnumDecl(EnumDecl),
    FunctionDecl(FunctionDecl),
    VarDecl(VarDecl),
    ParmVarDecl,
    // NamespaceDecl(NamespaceDecl),
    CallExpr(CallExpr),
    DeclRefExpr(DeclRefExpr),
    TranslationUnitDecl(TranslationUnitDecl),
    CompoundStmt(CompoundStmt),
    Other(Other),
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TranslationUnitDecl {
    pub loc: Option<clang_ast::SourceLocation>,
    pub range: Option<clang_ast::SourceRange>,
}

impl TranslationUnitDecl {
    async fn visit(
        &self,
        state: &mut GlobalVisitorState,
        unit_state: &mut ModuleVisitorState,
        inner: &Vec<Node>,
    ) -> Result<()> {
        for child in inner.iter() {
            match &child.kind {
                Clang::FunctionDecl(f) => {
                    f.visit(state, unit_state, child.id, &child.inner).await?
                }
                _ => {}
            }
        }

        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EnumConstantDecl {
    pub name: String,
    pub loc: Option<clang_ast::SourceLocation>,
    pub range: Option<clang_ast::SourceRange>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EnumDecl {
    pub name: Option<String>,
    pub loc: Option<clang_ast::SourceLocation>,
    pub range: Option<clang_ast::SourceRange>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FunctionDecl {
    pub name: Option<String>,
    pub loc: Option<clang_ast::SourceLocation>,
    pub range: Option<clang_ast::SourceRange>,
    pub inner: Option<Vec<Node>>,
    pub storage_class: Option<String>,
    pub previous_decl: Option<Id>,
}

impl FunctionDecl {
    fn get_symbol_state(&self, inner: &Vec<Node>) -> SymbolType {
        let compound_stmt: Vec<_> = inner
            .iter()
            .filter(|node| matches!(node.kind, Clang::CompoundStmt(_)))
            .collect();

        match compound_stmt.len() {
            0 => SymbolType::Declaration,
            1 => SymbolType::Definition,
            _ => panic!("Do not expect multiple compound statements"),
        }
    }

    fn get_symbol_scope(&self) -> SymbolScope {
        match self.storage_class.as_deref() {
            Some("static") => SymbolScope::Local,
            Some("extern") => SymbolScope::Global,
            None => SymbolScope::Global,
            _ => panic!("Unknown symbol scope {:?}", self.storage_class),
        }
    }

    fn extract_call_refs<'a>(&'a self, nodes: &'a Vec<Node>) -> impl Iterator<Item = &Node> + 'a {
        nodes
            .iter()
            // XXX: Normally I would like to save only the references, which are
            // actual function calls, but extract_filter is not correct for
            // recursive filter, for example if there is CallExpr inside of a
            // CallExpr. Or rather it will traverse nested CallExpr multiple
            // times. So, at least for now, I grab all function pointers. Which
            // is not too bad.
            //
            // .map(|node| {
            //     extract_filter(node, &|node: &Node| matches!(node.kind, Clang::CallExpr(_)))
            // }) .flatten()
            .map(|node| {
                extract_filter(node, &|node: &Node| {
                    matches!(node.kind, Clang::DeclRefExpr(_))
                })
            })
            .flatten()
    }

    async fn visit_references(
        &self,
        state: &mut GlobalVisitorState,
        unit_state: &mut ModuleVisitorState,
        declaration: &Declaration,
        inner: &Vec<Node>,
    ) -> Result<()> {
        for node in self.extract_call_refs(inner) {
            match &node.kind {
                Clang::DeclRefExpr(ref_expr) => {
                    let referenced_decl = ref_expr.referenced_decl.as_ref().unwrap();
                    let file_id = state
                        .extract_file_from_range(&ref_expr.range)
                        .await
                        .unwrap();
                    let occurrence = symbols::Occurrence::new(&ref_expr.range, file_id).unwrap();

                    match &referenced_decl.kind {
                        Clang::FunctionDecl(f) => {
                            // If the reference id is unknown, then the reference is
                            // also an implicit symbol declaration
                            let to_id = if let Some(symbol_id) =
                                unit_state.get_symbol(&referenced_decl.id)
                            {
                                *symbol_id
                            } else {
                                let name = f.name.as_ref().unwrap();
                                let symbol_scope = SymbolScope::Global;
                                // None, because this symbol is global
                                let module_id = None;
                                // Implicit symbol declaration
                                let symbol_type = SymbolType::Declaration;

                                let symbol = state
                                    .index
                                    .insert_symbol(name, module_id, symbol_scope)
                                    .await?;
                                unit_state.add_symbol(referenced_decl.id, symbol.id);

                                let declaration = Declaration::new(
                                    symbol.id,
                                    file_id,
                                    symbol_type,
                                    &ref_expr.range,
                                )
                                .unwrap();
                                state.index.add_declaration(declaration).await?;

                                symbol.id
                            };

                            state
                                .get_index()
                                .add_reference(declaration.id, to_id, &occurrence)
                                .await?;
                        }
                        Clang::ParmVarDecl | Clang::EnumConstantDecl(_) | Clang::VarDecl(_) => {}
                        _ => {
                            panic!("Impossible node kind: {:#?}", ref_expr);
                        }
                    }
                }
                _ => {
                    panic!("Impossible node kind");
                }
            }
        }

        Ok(())
    }

    async fn visit(
        &self,
        state: &mut GlobalVisitorState,
        unit_state: &mut ModuleVisitorState,
        id: Id,
        inner: &Vec<Node>,
    ) -> Result<()> {
        let clang_range = self.range.clone();
        let file_id = state.extract_file_from_range(&clang_range).await.unwrap();

        let name = self.name.as_ref().unwrap();

        let symbol_type = self.get_symbol_state(inner);
        let symbol_scope = self.get_symbol_scope();

        let module_id = if let SymbolScope::Local = symbol_scope {
            Some(unit_state.module_id)
        } else {
            None
        };

        let symbol = state
            .index
            .insert_symbol(name, module_id, symbol_scope)
            .await?;

        let declaration = Declaration::new(symbol.id, file_id, symbol_type, &clang_range).unwrap();
        let declaration = state.index.add_declaration(declaration).await?;
        unit_state.add_symbol(id, symbol.id);

        self.visit_references(state, unit_state, &declaration, inner)
            .await?;

        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct VarDecl {
    pub name: Option<String>,
    pub loc: Option<clang_ast::SourceLocation>,
    pub range: Option<clang_ast::SourceRange>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CompoundStmt {
    pub name: Option<String>,
    pub loc: Option<clang_ast::SourceLocation>,
    pub range: Option<clang_ast::SourceRange>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CallExpr {
    pub name: Option<String>,
    pub loc: Option<clang_ast::SourceLocation>,
    pub range: Option<clang_ast::SourceRange>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DeclRefExpr {
    pub name: Option<String>,
    pub loc: Option<clang_ast::SourceLocation>,
    pub range: Option<clang_ast::SourceRange>,
    pub referenced_decl: Option<Box<Node>>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct NamespaceDecl {
    pub name: Option<String>,
    pub loc: Option<clang_ast::SourceLocation>,
    pub range: Option<clang_ast::SourceRange>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Other {
    pub loc: Option<clang_ast::SourceLocation>,
    pub range: Option<clang_ast::SourceRange>,
}

/// Run Clang with parameters for generating the AST, where [`clang`] is the
/// path to the clang binary.
pub async fn run_clang_ast(clang: &str, c: CompileCommand) -> anyhow::Result<(String, Node)> {
    let mut arguments = if let Some(ref command) = c.command {
        shell_words::split(command).expect("Failed to parse command")
    } else if let Some(arguments) = c.arguments {
        arguments
    } else {
        return Err(anyhow!(
            "Either command or arguments must be defined for file: {}",
            c.file
        ));
    };

    let ast_file;
    if let Some(i) = arguments.iter().position(|opt| *opt == "-o") {
        // Replace option of type "-o outfile"
        ast_file = format!("{}/{}.ast", c.directory, arguments[i + 1]);
        // arguments[i + 1] = output;
    } else if let Some(i) = arguments.iter().position(|opt| opt.starts_with("-o")) {
        // Replace option of type "-ooutfile"
        ast_file = format!("{}/{}.ast", c.directory, &arguments[i + 1][2..]);
        // arguments[i] = format!("-o{}", output);
    } else {
        ast_file = format!("{}/{}.ast", c.directory, c.file);
        // arguments.push(format!("-o{}", output));
    }

    arguments = vec![
        "-Xclang".to_string(),
        "-ast-dump=json".to_string(),
        "-fsyntax-only".to_string(),
    ]
    .into_iter()
    .chain(
        arguments
            .drain(1..) // Remove path to the compiler
            .filter(|arg| arg != "-Werror")
            .filter(|arg| arg != "-c")
            .filter(|arg| arg != "-g")
            .filter(|arg| !arg.starts_with("-f"))
            .filter(|arg| !arg.starts_with("-m")),
    )
    .collect();

    let output = Command::new(clang)
        .current_dir(c.directory)
        .args(arguments)
        .output()
        .await?;

    let json = String::from_utf8(output.stdout)?;

    if !output.status.success() {
        let stderr = String::from_utf8(output.stderr)?;
        return Err(anyhow!("Error: {}", stderr));
    }

    std::fs::write("ast.json", json.clone())?;
    let node: Node = serde_json::from_str(&json)?;
    // std::fs::write("node", format!("{:#?}", node))?;

    Ok((c.file, node))
}

#[derive(Debug, Deserialize, Clone, Eq, PartialEq, Hash)]
struct UnresolvedChild {
    from: SymbolId,
    to: SymbolId,
    occurrence: symbols::Occurrence,
}

pub struct GlobalVisitorState {
    index: Index,
    project: String,
    language: String,
}

impl GlobalVisitorState {
    pub fn new(index: Index) -> Self {
        GlobalVisitorState {
            index: index,
            project: "test".to_string(),
            language: "cxx".to_string(),
        }
    }

    async fn extract_file_from_range(
        &self,
        range: &Option<clang_ast::SourceRange>,
    ) -> Result<FileId> {
        let file =
            symbols::Occurrence::get_file(range).ok_or(anyhow!("Range does not provide file"))?;
        let file_string = file.into_os_string().into_string().unwrap();

        self.index
            .create_or_get_fileid(&file_string, &self.project, &self.language)
            .await
    }

    pub async fn extract_symbol_map_root(&mut self, module: &str, root: Node) -> Result<()> {
        let node = if let Clang::TranslationUnitDecl(node) = root.kind {
            node
        } else {
            bail!("Not implemented");
        };

        let module_id = self
            .index
            .create_or_get_fileid(&module, &self.project, &self.language)
            .await?;

        let mut unit_state = ModuleVisitorState::new(module_id);
        node.visit(self, &mut unit_state, &root.inner).await?;

        Ok(())
    }

    pub fn get_index(&self) -> &Index {
        &self.index
    }
}

impl Into<Index> for GlobalVisitorState {
    fn into(self) -> Index {
        self.index
    }
}

struct ModuleVisitorState {
    module_id: FileId,
    /// A map of registered symbols with the list of related symbols. Related
    /// symbols are the one which point to each other using [`previous_decl`]
    symbol_ids: HashMap<Id, SymbolId>,
}

impl ModuleVisitorState {
    fn new(module_id: FileId) -> Self {
        Self {
            module_id,
            symbol_ids: HashMap::new(),
        }
    }

    fn add_symbol(&mut self, clang_id: Id, symbol_id: SymbolId) {
        self.symbol_ids.insert(clang_id, symbol_id);
    }

    fn get_symbol(&self, id: &Id) -> Option<&SymbolId> {
        self.symbol_ids.get(id)
    }
}
