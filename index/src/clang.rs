use std::{collections::HashMap, path::Path};

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
                        .extract_file_from_range(unit_state, &ref_expr.range)
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
        let file_id = state
            .extract_file_from_range(unit_state, &clang_range)
            .await
            .unwrap();

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

#[derive(PartialEq)]
enum Language {
    C,
    Cxx,
    Asm,
}

impl Language {
    fn parse_path(path: &str) -> Option<Language> {
        let lower_path = path.to_string().to_lowercase();

        if lower_path.ends_with(".c") {
            Some(Language::C)
        } else if lower_path.ends_with(".cxx") {
            Some(Language::Cxx)
        } else if lower_path.ends_with(".cc") {
            Some(Language::Cxx)
        } else if lower_path.ends_with(".c++") {
            Some(Language::Cxx)
        } else if lower_path.ends_with(".s") {
            Some(Language::Asm)
        } else {
            None
        }
    }
}

fn preprocess(arguments: Vec<String>) -> anyhow::Result<Vec<String>> {
    let mut res_arguments = Vec::new();
    let mut skip_next = false;
    for arg in arguments.into_iter().skip(1) {
        if skip_next == true {
            skip_next = false;
            continue;
        } else if arg.starts_with("-Wno-") {
            // We keep warning relaxations
        } else if arg.starts_with("-W") {
            // We skip extra warnings
            continue;
        } else if arg.starts_with("-f") {
            continue;
        } else if arg.starts_with("-m") {
            continue;
        } else if arg == "-g" {
            continue;
        } else if arg == "-c" {
            continue;
        } else if arg == "-o" {
            skip_next = true;
            continue;
        }

        res_arguments.push(arg);
    }

    Ok(res_arguments)
}

#[derive(Debug)]
pub struct ParsedNode {
    pub module: String,
    pub root: String,
    pub node: Node,
}

impl ParsedNode {
    fn new(c: CompileCommand, node: Node) -> Result<Self> {
        let file = Path::new(&c.file);
        let dir = Path::new(&c.directory);
        if let Ok(module_path) = file.strip_prefix(dir) {
            Ok(ParsedNode {
                module: c.file.clone(),
                root: module_path.to_str().unwrap().to_string(),
                node: node,
            })
        } else {
            Err(anyhow!(
                "File {} should reside within the module directory {}",
                c.file,
                c.directory
            ))
        }
    }
}

/// Run Clang with parameters for generating the AST, where [`clang`] is the
/// path to the clang binary.
pub async fn run_clang_ast(clang: &str, c: CompileCommand) -> anyhow::Result<ParsedNode> {
    // println!("{:#?}", c);
    let language = Language::parse_path(&c.file);

    match language {
        Some(Language::C) => Language::C,
        Some(_) => return Err(anyhow!("Only C files are supported for now")),
        None => return Err(anyhow!("Failed to parse language")),
    };

    let mut arguments = if let Some(ref command) = c.command {
        shell_words::split(command).expect("Failed to parse command")
    } else if let Some(arguments) = c.arguments.clone() {
        arguments
    } else {
        return Err(anyhow!(
            "Either command or arguments must be defined for file: {}",
            c.file
        ));
    };

    // let ast_file;
    // if let Some(i) = arguments.iter().position(|opt| *opt == "-o") {
    //     // Replace option of type "-o outfile"
    //     ast_file = format!("{}/{}.ast", c.directory, arguments[i + 1]);
    //     // arguments[i + 1] = output;
    // } else if let Some(i) = arguments.iter().position(|opt| opt.starts_with("-o")) {
    //     // Replace option of type "-ooutfile"
    //     ast_file = format!("{}/{}.ast", c.directory, &arguments[i + 1][2..]);
    //     // arguments[i] = format!("-o{}", output);
    // } else {
    //     ast_file = format!("{}/{}.ast", c.directory, c.file);
    //     // arguments.push(format!("-o{}", output));
    // }

    let filtered_args = preprocess(arguments)?;

    arguments = vec![
        "-Xclang".to_string(),
        "-ast-dump=json".to_string(),
        "-fsyntax-only".to_string(),
    ]
    .into_iter()
    .chain(filtered_args)
    .collect();

    let output = Command::new(clang)
        .current_dir(&c.directory)
        .args(arguments)
        .output()
        .await?;

    let json = String::from_utf8(output.stdout)?;

    if !output.status.success() {
        let stderr = String::from_utf8(output.stderr)?;
        return Err(anyhow!("Error: {}", stderr));
    }

    std::fs::write("ast.json", json.clone())?;
    let mut deserializer = serde_json::Deserializer::from_str(&json);
    deserializer.disable_recursion_limit();
    let deserializer = serde_stacker::Deserializer::new(&mut deserializer);
    let node = Node::deserialize(deserializer).unwrap();
    // std::fs::write("node", format!("{:#?}", node))?;

    ParsedNode::new(c, node)
}

#[derive(Debug, Deserialize, Clone, Eq, PartialEq, Hash)]
struct UnresolvedChild {
    from: SymbolId,
    to: SymbolId,
    occurrence: symbols::Occurrence,
}

pub struct GlobalVisitorState {
    index: Index,
    module: String,
    language: String,
}

impl GlobalVisitorState {
    pub fn new(index: Index, module: &str) -> Self {
        GlobalVisitorState {
            index: index,
            module: module.to_string(),
            language: "cxx".to_string(),
        }
    }

    async fn extract_file_from_range(
        &self,
        unit_state: &mut ModuleVisitorState,
        range: &Option<clang_ast::SourceRange>,
    ) -> Result<FileId> {
        let file =
            symbols::Occurrence::get_file(range).ok_or(anyhow!("Range does not provide file"))?;
        let file_string = file.clone().into_os_string().into_string().unwrap();
        let module_path = file
            .as_path()
            .strip_prefix(Path::new(&unit_state.root_dir))?;

        self.index
            .create_or_get_fileid(
                &self.module,
                module_path.to_str().unwrap(),
                &file_string,
                &self.language,
            )
            .await
    }

    pub async fn extract_symbol_map_root(&mut self, root_node: ParsedNode) -> Result<()> {
        let root_dir = root_node
            .module
            .strip_suffix(&root_node.root)
            .ok_or(anyhow!(
                "Expected {} to be suffix of {}",
                root_node.root,
                root_node.module
            ))?;
        let node = if let Clang::TranslationUnitDecl(node) = root_node.node.kind {
            node
        } else {
            bail!("Not implemented");
        };

        let module_id = self
            .index
            .create_or_get_fileid(
                &self.module,
                &root_node.root,
                &root_node.module,
                &self.language,
            )
            .await?;

        let mut unit_state = ModuleVisitorState::new(module_id, root_dir);
        node.visit(self, &mut unit_state, &root_node.node.inner)
            .await?;

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
    root_dir: String,

    /// A map of registered symbols with the list of related symbols. Related
    /// symbols are the one which point to each other using [`previous_decl`]
    symbol_ids: HashMap<Id, SymbolId>,
}

impl ModuleVisitorState {
    fn new(module_id: FileId, root_dir: &str) -> Self {
        Self {
            module_id,
            root_dir: root_dir.to_string(),
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
