use std::fs::File;

use anyhow::anyhow;
use clap::Parser;
use serde::{Serialize, Deserialize};
use tokio::{sync::mpsc, process::Command};

/// Indexer for askl
#[derive(Parser, Debug)]
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
    trim: Option<usize>
}

#[derive(Serialize, Deserialize, Debug)]
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
    pub range: Option<clang_ast::SourceRange>
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
    let inner : Vec<Node> = root.inner
        .into_iter()
        .map(|node| node_simplify(node))
        .flatten()
        .collect();
    match &root.kind {
        Clang::DeclRefExpr(ref_expr) => {
            if let Some(referenced_decl) = &ref_expr.referenced_decl {
                if let Clang::FunctionDecl(_) = &referenced_decl.kind {
                    return vec![Node{
                        id: root.id,
                        kind: root.kind,
                        inner: inner,
                    }];
                }
            }
            vec![]
        },
        Clang::FunctionDecl(_) => {
            if inner.len() > 0 {
                vec![Node{
                    id: root.id,
                    kind: root.kind,
                    inner: inner,
                }]
            } else {
                inner
            }
        },
        Clang::TranslationUnitDecl => {
            vec![Node{
                id: root.id,
                kind: root.kind,
                inner: inner,
            }]
        },
        Clang::Other | Clang::CompoundStmt=> {
            inner
        },
    }
}

async fn run_ast_gen(args: &Args, c: CompileCommand) -> anyhow::Result<(String, Node)> {

    let mut arguments = if let Some(ref command) = c.command {
        shell_words::split(command).expect("Failed to parse command")
    } else if let Some(arguments) = c.arguments {
        arguments
    } else {
        return Err(anyhow!("Either command or arguments must be defined for file: {}", c.file));
    };

    println!("{:?}", arguments);
    let output;
    if let Some(i) = arguments.iter().position(|opt| *opt == "-o") {
        // Replace option of type "-o outfile"
        output = format!("{}/{}.pch", c.directory, arguments[i + 1]);
        // arguments[i + 1] = output;
        println!("!: {}", output);
    } else if let Some(i) = arguments.iter().position(|opt| opt.starts_with("-o")) {
        // Replace option of type "-ooutfile"
        output = format!("{}/{}.pch", c.directory, &arguments[i + 1][2..]);
        // arguments[i] = format!("-o{}", output);
        println!("$: {}", output);
    } else {
        output = format!("{}/{}.pch", c.directory, c.file);
        // arguments.push(format!("-o{}", output));
        println!("#: {}", output);
    }

    if let Some(i) = arguments.iter().position(|opt| *opt == "-c") {
        // Remove option "-c"
        arguments.remove(i);
    }

    if let Some(i) = arguments.iter().position(|opt| *opt == "-g") {
        // Remove option "-c"
        arguments.remove(i);
    }

    // Remove path to the compiler
    arguments.remove(0);

    // let arguments = [
    //     vec![
    //         "-Xclang".to_string(),
    //         "-emit-pch".to_string(),
    //         "-fsyntax-only".to_string()
    //     ],
    //     arguments
    // ].concat();

    let arguments = [
        vec![
            "-Xclang".to_string(),
            "-ast-dump=json".to_string(),
            "-fsyntax-only".to_string()
        ],
        arguments
    ].concat();

    println!("{:?}", arguments.join(" "));

    let output = Command::new(args.clang.clone())
        .current_dir(c.directory)
        .args(arguments)
        .output()
        .await?;

    println!("{:?}", c.file);
    let json = String::from_utf8(output.stdout)?;

    let node : Node = serde_json::from_str(&json)?;

    // let simple_node = node;
    let simple_node = node_simplify(node).pop().unwrap();

    Ok((c.file, simple_node))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();
    let args = Args::parse();

    let file = File::open(&args.compile_commands)
        .expect("file should open read only");
    let mut compile_commands: Vec<CompileCommand> = serde_json::from_reader(file)
        .expect("file should be proper JSON");

    if let Some(trim) = args.trim {
        compile_commands.truncate(trim);
    }

    let (tx, mut rx) = mpsc::channel(args.parallelism);

    tokio::spawn(async move {
        for c in compile_commands {
            tx.send(c).await.unwrap();
        }
    });

    let mut first = true;
    // println!("[");
    while let Some(c) = rx.recv().await {
        let (file, node) = run_ast_gen(&args, c).await?;

        if first {
            first = false;
        } else {
            // println!(",");
        }
        print!(r#""{}": {}"#, file, serde_json::to_string_pretty(&node)?);
    }
    // println!("\n]");

    Ok(())
}