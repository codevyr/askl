use std::{fs::File, sync::Arc};

use anyhow::anyhow;
use askl::symbols::{Symbol, SymbolId, SymbolMap, Symbols};
use clap::Parser;
use indicatif::ProgressBar;
use serde::{Deserialize, Serialize};
use tokio::{process::Command, sync::Semaphore};

/// Indexer for askl
#[derive(Parser, Debug, Clone)]
#[clap(author, version, about, long_about = None)]
struct Args {
    // Path to compile command to get the list of source files
    #[clap(value_name = "FILE")]
    compile_commands: String,

    /// Command to invoke Clang
    #[clap(short, long, default_value = "clang")]
    clang: String,

    /// Maximum parallelism
    #[clap(short, long, default_value = "1")]
    parallelism: usize,

    // Limit how many files can be processed
    #[clap(long)]
    trim: Option<usize>,
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

#[derive(Serialize, Deserialize, Debug)]
pub enum Clang {
    // EnumConstantDecl(EnumConstantDecl),
    // EnumDecl(EnumDecl),
    FunctionDecl(FunctionDecl),
    // NamespaceDecl(NamespaceDecl),
    DeclRefExpr(DeclRefExpr),
    TranslationUnitDecl,
    CompoundStmt,
    Other,
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

fn node_simplify(root: Node) -> Vec<Node> {
    let inner: Vec<Node> = root
        .inner
        .into_iter()
        .map(|node| node_simplify(node))
        .flatten()
        .collect();
    match &root.kind {
        Clang::DeclRefExpr(ref_expr) => {
            if let Some(referenced_decl) = &ref_expr.referenced_decl {
                if let Clang::FunctionDecl(_) = &referenced_decl.kind {
                    return vec![Node {
                        id: root.id,
                        kind: root.kind,
                        inner: inner,
                    }];
                }
            }
            vec![]
        }
        Clang::FunctionDecl(_) => {
            vec![Node {
                id: root.id,
                kind: root.kind,
                inner: inner,
            }]
        }
        Clang::TranslationUnitDecl => {
            vec![Node {
                id: root.id,
                kind: root.kind,
                inner: inner,
            }]
        }
        Clang::Other | Clang::CompoundStmt => inner,
    }
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

    let node: Node = serde_json::from_str(&json)?;

    let simple_node = node_simplify(node).pop().unwrap();

    Ok((ast_file, simple_node))
}

async fn parse_all(
    args: Args,
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

    let outputs = parse_all(args, compile_commands).await;

    let all_ast = outputs
        .into_iter()
        .map(|r| {
            if let Err(err) = &r {
                println!("{:?}", err);
            }
            r
        })
        .filter(|r| r.is_ok())
        .map(|r| r.unwrap())
        .map(|(_, node)| node)
        .reduce(|mut acc, node| {
            acc.inner.extend(node.inner);
            Node {
                id: acc.id,
                kind: acc.kind,
                inner: acc.inner,
            }
        })
        .unwrap();

    let mut symbol_map = SymbolMap::new();
    for node in all_ast.inner {
        if let Clang::FunctionDecl(f) = node.kind {
            let children = node
                .inner
                .iter()
                .filter_map(|i| {
                    if let Clang::DeclRefExpr(r) = &i.kind {
                        if let Some(ref_decl) = &r.referenced_decl {
                            if let Clang::FunctionDecl(f) = &ref_decl.kind {
                                if let Some(name) = &f.name {
                                    return Some(SymbolId::new(name.clone()));
                                }
                            }
                        }
                    }
                    None
                })
                .collect();

            symbol_map.add(
                SymbolId::new(f.name.clone().unwrap()),
                Symbol {
                    name: f.name.clone().unwrap(),
                    ranges: vec![f.range.unwrap()],
                    children: children,
                },
            );
        }
    }

    std::fs::write(
        "symbol_map.json",
        serde_json::to_string_pretty(&symbol_map).unwrap(),
    )
    .unwrap();
    Ok(())
}
