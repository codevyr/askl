use std::{fs::File, path::PathBuf, sync::Arc};

use anyhow::{anyhow, Result};
use askl::symbols::{Occurence, Symbol, SymbolChild, SymbolId, SymbolMap, Symbols};
use clap::Parser;
use indicatif::ProgressBar;
use log::debug;
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
    Other,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TranslationUnitDecl;

impl TranslationUnitDecl {
    fn extract_symbol_map(&self, inner: &Vec<Node>) -> Option<SymbolMap> {
        inner
            .iter()
            .filter_map(|child| match &child.kind {
                Clang::FunctionDecl(f) => Some(f.extract_symbol_map(&child.inner)),
                _ => None,
            })
            .map(|s| s.unwrap())
            .reduce(|acc, next| acc.merge(next))
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EnumConstantDecl {
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EnumDecl {
    pub name: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct FunctionDecl {
    pub name: Option<String>,
    pub loc: Option<clang_ast::SourceLocation>,
    pub range: Option<clang_ast::SourceRange>,
    pub inner: Option<Vec<Node>>,
}

impl FunctionDecl {
    fn extract_symbol_map(&self, inner: &Vec<Node>) -> Option<SymbolMap> {
        let clang_range = self.range.clone().unwrap();
        let range =
            if clang_range.begin.spelling_loc.is_some() && clang_range.end.spelling_loc.is_some() {
                let file = clang_range
                    .begin
                    .spelling_loc
                    .as_ref()
                    .unwrap()
                    .file
                    .clone()
                    .to_string();
                if file == "" {
                    None
                } else {
                    Some(Occurence::new(file.into(), clang_range))
                }
            } else {
                None
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
                        Clang::FunctionDecl(f) => Some(SymbolChild {
                            symbol_id: SymbolId::new(f.name.as_ref().unwrap().clone()),
                            occurence: Some(Occurence::new(
                                PathBuf::from(
                                    ref_expr
                                        .range
                                        .as_ref()
                                        .unwrap()
                                        .begin
                                        .expansion_loc
                                        .as_ref()
                                        .unwrap()
                                        .file
                                        .clone()
                                        .to_string(),
                                ),
                                ref_expr.range.as_ref().unwrap().clone(),
                            )),
                        }),
                        // Clang::VarDecl(v) => {
                        //     Some(SymbolChild {
                        //         symbol_id: SymbolId::new(v.name.as_ref().unwrap().clone()),
                        //         occurence: Occurence::new(
                        //             ref_expr.range.as_ref().unwrap().begin.expansion_loc.as_ref().unwrap().file.clone().to_string(),
                        //             ref_expr.range.as_ref().unwrap().clone(),
                        //         ),
                        //     })
                        // }
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

        let mut symbol_map = SymbolMap::new();
        symbol_map.add(
            SymbolId::new(self.name.clone().unwrap()),
            Symbol {
                name: self.name.clone().unwrap(),
                ranges: range.into_iter().collect(),
                children: children,
            },
        );

        Some(symbol_map)
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

    // Dump the AST to a file
    std::fs::write("ast.json", &json)?;

    let node: Node = serde_json::from_str(&json)?;

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
                println!("{} in {:?}", err, c);
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

fn extract_symbol_map_root(root: Node) -> Result<SymbolMap> {
    match root.kind {
        Clang::TranslationUnitDecl(node) => Ok(node
            .extract_symbol_map(&root.inner)
            .unwrap_or_else(|| SymbolMap::new())),
        _ => Err(anyhow!("Not implemented")),
    }
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

    let symbol_map = outputs
        .into_iter()
        .map(|r| {
            if let Err(err) = &r {
                println!("Failed parsing: {:?}", err);
            }
            r
        })
        .filter(|r| r.is_ok())
        .map(|r| r.unwrap())
        .map(|(_, node)| node)
        .map(extract_symbol_map_root)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .reduce(|acc, next| acc.merge(next))
        .unwrap();

    std::fs::write(
        args.symbol_map,
        serde_json::to_string_pretty(&symbol_map).unwrap(),
    )
    .unwrap();
    Ok(())
}
