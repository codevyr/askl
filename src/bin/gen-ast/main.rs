use std::{
    collections::{HashMap, HashSet},
    fs::File,
    mem::replace,
    sync::Arc,
};

use anyhow::{anyhow, bail, Result};
use askl::symbols::{Occurence, Symbol, SymbolId, SymbolMap, SymbolRefs, Symbols, FileId};
use clap::Parser;
use indicatif::ProgressBar;
use serde::{Deserialize, Serialize};
use tokio::{process::Command, sync::Semaphore};

/// Indexer for askl
#[derive(Parser, Debug, Clone)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Path to compile command to get the list of source files
    #[clap(value_name = "FILE")]
    compile_commands: String,

    /// Command to invoke Clang
    #[clap(short, long, default_value = "clang")]
    clang: String,

    /// Maximum parallelism
    #[clap(short, long, default_value = "1")]
    parallelism: usize,

    /// Limit how many files can be processed
    #[clap(long)]
    trim: Option<usize>,

    /// Output file to store the resulting symbol map
    #[clap(short, long, default_value = "symbol_map.json")]
    symbol_map: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct CompileCommand {
    arguments: Option<Vec<String>>,
    command: Option<String>,
    directory: String,
    file: String,
    output: Option<String>,
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
    fn extract_symbol_map(&self, state: &mut VisitorState, inner: &Vec<Node>) -> Result<()> {
        inner.iter().try_for_each(|child| match &child.kind {
            Clang::FunctionDecl(f) => f.extract_symbol_map(state, &child.inner),
            _ => Ok(()),
        })
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
    fn extract_symbol_map(&self, state: &mut VisitorState, inner: &Vec<Node>) -> Result<()> {
        let clang_range = self.range.clone();
        let file_id = state.extract_file_from_range(&clang_range).unwrap();

        let range = Occurence::new(&clang_range, file_id).unwrap();
        let name = self.name.clone().unwrap();

        let parent_id = if let Some(symbol) = state.symbol_map.find_mut(&name) {
            symbol.ranges.insert(range);
            symbol.id
        } else {
            let next_id = state.next_symbol_id();
            let new_symbol = Symbol {
                id: next_id,
                name: name,
                ranges: HashSet::from([range]),
                children: SymbolRefs::new(),
            };

            state.symbol_map.add(next_id, new_symbol);
            next_id
        };

        let children: Vec<_> = inner
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
            .flatten()
            .filter_map(|node| match &node.kind {
                Clang::DeclRefExpr(ref_expr) => {
                    let referenced_decl = ref_expr.referenced_decl.as_ref().unwrap();
                    match &referenced_decl.kind {
                        Clang::FunctionDecl(f) => {
                            let name = f.name.as_ref().unwrap().clone();
                            Some(UnresolvedChild {
                                parent_id: parent_id,
                                child_name: name,
                                occurence: ref_expr.range.clone(),
                            })
                        }
                        Clang::ParmVarDecl | Clang::EnumConstantDecl(_) | Clang::VarDecl(_) => None,
                        _ => {
                            panic!("Impossible node kind: {:#?}", ref_expr);
                        }
                    }
                }
                _ => {
                    panic!("Impossible node kind");
                }
            })
            .collect();

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

async fn run_ast_gen(args: Args, c: CompileCommand) -> anyhow::Result<(String, Node)> {
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

    let output = Command::new(args.clang.clone())
        .current_dir(c.directory)
        .args(arguments)
        .output()
        .await?;

    let json = String::from_utf8(output.stdout)?;

    if !output.status.success() {
        let stderr = String::from_utf8(output.stderr)?;
        return Err(anyhow!("Error: {}", stderr));
    }

    // std::fs::write("ast.json", json.clone())?;
    let node: Node = serde_json::from_str(&json)?;
    // std::fs::write("node", format!("{:#?}", node))?;

    Ok((ast_file, node))
}

async fn parse_all(
    args: &Args,
    compile_commands: Vec<CompileCommand>,
) -> Vec<anyhow::Result<(String, Node)>> {
    let sem = Arc::new(Semaphore::new(args.parallelism));
    let mut tasks = Vec::with_capacity(compile_commands.len());
    let pb = ProgressBar::new(compile_commands.len() as u64);
    for c in compile_commands {
        let permit = Arc::clone(&sem).acquire_owned().await.unwrap();
        let pb = pb.clone();
        let _args = args.clone();
        tasks.push(tokio::spawn(async move {
            pb.inc(1);
            let res = run_ast_gen(_args, c.clone()).await;
            if let Err(err) = &res {
                println!("Run AST {} in {:?}", err, c);
            }
            drop(permit);
            res
        }));
    }

    let mut outputs = Vec::<anyhow::Result<(String, Node)>>::with_capacity(tasks.len());
    for task in tasks {
        outputs.push(task.await.unwrap());
    }

    outputs
}

#[derive(Debug, Clone)]
struct UnresolvedChild {
    parent_id: SymbolId,
    child_name: String,
    occurence: Option<clang_ast::SourceRange>,
}

struct VisitorState {
    next_symbol_id: u64,
    next_file_id: u64,
    unresolved_children: HashMap<String, Vec<UnresolvedChild>>,
    symbol_map: SymbolMap,
}

impl VisitorState {
    fn new() -> Self {
        VisitorState {
            next_symbol_id: 1,
            next_file_id: 1,
            unresolved_children: HashMap::new(),
            symbol_map: SymbolMap::new(),
        }
    }

    fn next_symbol_id(&mut self) -> SymbolId {
        let next_id = SymbolId::new(self.next_symbol_id);
        self.next_symbol_id = self.next_symbol_id + 1;
        return next_id;
    }

    fn next_file_id(&mut self) -> FileId {
        let next_id = FileId::new(self.next_file_id);
        self.next_file_id = self.next_file_id + 1;
        return next_id;
    }

    fn add_unresolved_children(&mut self, children: Vec<UnresolvedChild>) {
        for child in children {
            self.unresolved_children
                .entry(child.child_name.clone())
                .and_modify(|v| v.push(child.clone()))
                .or_insert_with(|| vec![child]);
        }
    }

    fn extract_file_from_range(&mut self, range: &Option<clang_ast::SourceRange>) -> Option<FileId> {
        let file = Occurence::get_file(range)?;
        let file_string = file.into_os_string().into_string().unwrap();

        if let Some(id) =  self.symbol_map.get_file_id(file_string.clone()){
            Some(id)
        } else {
            let id = self.next_file_id();
            self.symbol_map.set_file_id(id, file_string);
            Some(id)
        }        
    }
}

fn extract_symbol_map_root(root: Node, state: &mut VisitorState) -> Result<()> {
    match root.kind {
        Clang::TranslationUnitDecl(node) => node.extract_symbol_map(state, &root.inner)?,
        _ => bail!("Not implemented"),
    };

    let unresolved_children = replace(&mut state.unresolved_children, HashMap::new());
    for (child_name, unresolved) in unresolved_children {
        let child = state
            .symbol_map
            .symbols
            .iter()
            .find(|(_, s)| s.name == *child_name)
            .unwrap()
            .1
            .clone();
        for u in unresolved {
            let file_id = state.extract_file_from_range(&u.occurence).unwrap();

            state
                .symbol_map
                .symbols
                .entry(u.parent_id)
                .and_modify(|s| {

                    s.add_child(child.id, Occurence::new(&u.occurence, file_id).unwrap());
                })
                .or_insert_with(|| panic!("Did not find the parent"));
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();
    let args = Args::parse();

    let file = File::open(&args.compile_commands).expect("file should open read only");
    let mut compile_commands: Vec<CompileCommand> =
        serde_json::from_reader(file).expect("file should be proper JSON");

    if let Some(trim) = args.trim {
        compile_commands.truncate(trim);
    }

    let outputs = parse_all(&args, compile_commands).await;
    let mut state = VisitorState::new();
    outputs
        .into_iter()
        .filter(|r| r.is_ok())
        .map(|r| r.unwrap())
        .map(|(_, node)| node)
        .map(|root| extract_symbol_map_root(root, &mut state))
        .collect::<Result<Vec<_>>>()?;

    std::fs::write(
        args.symbol_map,
        serde_json::to_string_pretty(&state.symbol_map).unwrap(),
    )
    .unwrap();
    Ok(())
}
