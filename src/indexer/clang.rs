use std::collections::{HashMap, HashSet};

use crate::{
    index::Index,
    symbols::{FileId, Occurrence, SymbolId, SymbolType},
};
use anyhow::{anyhow, bail, Result};
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
    async fn extract_symbol_map(&self, state: &mut VisitorState, inner: &Vec<Node>) -> Result<()> {
        for child in inner.iter() {
            match &child.kind {
                Clang::FunctionDecl(f) => f.extract_symbol_map(state, &child.inner).await?,
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
pub struct FunctionDecl {
    pub name: Option<String>,
    pub loc: Option<clang_ast::SourceLocation>,
    pub range: Option<clang_ast::SourceRange>,
    pub inner: Option<Vec<Node>>,
}

impl FunctionDecl {
    async fn extract_symbol_map(&self, state: &mut VisitorState, inner: &Vec<Node>) -> Result<()> {
        let clang_range = self.range.clone();
        let file_id = state.extract_file_from_range(&clang_range).await.unwrap();

        let range = Occurrence::new(&clang_range, file_id).unwrap();
        let name = self.name.clone().unwrap();

        let parent_id = state
            .index
            .create_or_get_symbolid(&name, SymbolType::Definition, range)
            .await?;

        let inner_nodes = inner
            .iter()
            .map(|node| {
                extract_filter(node, &|node: &Node| matches!(node.kind, Clang::CallExpr(_)))
            })
            .flatten()
            .map(|node| {
                extract_filter(node, &|node: &Node| {
                    matches!(node.kind, Clang::DeclRefExpr(_))
                })
            })
            .flatten();

        let mut children = Vec::new();
        for node in inner_nodes {
            match &node.kind {
                Clang::DeclRefExpr(ref_expr) => {
                    let referenced_decl = ref_expr.referenced_decl.as_ref().unwrap();
                    let file_id = state
                        .extract_file_from_range(&ref_expr.range)
                        .await
                        .unwrap();
                    let occurrence = Occurrence::new(&ref_expr.range, file_id).unwrap();

                    match &referenced_decl.kind {
                        Clang::FunctionDecl(f) => {
                            let child_name = f.name.as_ref().unwrap().clone();
                            children.push(UnresolvedChild {
                                parent_id,
                                child_name,
                                occurrence,
                            })
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

        state.add_unresolved_children(children);

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
            .filter(|arg| !arg.starts_with("-f")),
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

    Ok((ast_file, node))
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq, Hash)]
struct UnresolvedChild {
    parent_id: SymbolId,
    child_name: String,
    occurrence: Occurrence,
}

pub struct VisitorState {
    unresolved_children: HashMap<String, HashSet<UnresolvedChild>>,
    index: Index,
    project: String,
    language: String,
}

impl VisitorState {
    pub fn new(index: Index) -> Self {
        VisitorState {
            unresolved_children: HashMap::new(),
            index: index,
            project: "test".to_string(),
            language: "cxx".to_string(),
        }
    }

    fn add_unresolved_children(&mut self, children: Vec<UnresolvedChild>) {
        for child in children {
            self.unresolved_children
                .entry(child.child_name.clone())
                .and_modify(|v| {
                    v.insert(child.clone());
                })
                .or_insert_with(|| HashSet::from([child]));
        }
    }

    async fn extract_file_from_range(
        &self,
        range: &Option<clang_ast::SourceRange>,
    ) -> Result<FileId> {
        let file = Occurrence::get_file(range).ok_or(anyhow!("Range does not provide file"))?;
        let file_string = file.into_os_string().into_string().unwrap();

        self.index
            .create_or_get_fileid(&file_string, &self.project, &self.language)
            .await
    }

    pub async fn handle_unresolved_symbols(&self) -> Result<()> {
        for (child_name, unresolved) in self.unresolved_children.iter() {
            let resolved_children = self.index.find_symbols(&child_name).await?;
            for resolved_child in resolved_children {
                for u in unresolved.iter() {
                    let res = self
                        .index
                        .add_reference(u.parent_id, resolved_child.id, &u.occurrence)
                        .await;
                    if res.is_err() {
                        log::error!("{:#?}", unresolved);
                    }
                    res?;
                }
            }
        }

        Ok(())
    }

    pub async fn extract_symbol_map_root(&mut self, root: Node) -> Result<()> {
        match root.kind {
            Clang::TranslationUnitDecl(node) => node.extract_symbol_map(self, &root.inner).await?,
            _ => bail!("Not implemented"),
        };

        Ok(())
    }
}
