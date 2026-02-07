use anyhow::Result;
use clap::{Parser, Subcommand};
use pieskieo_core::{PieskieoDb, VectorParams};
use pieskieo_server;
use serde_json::Value;
use std::io::{self, Write};
use std::path::PathBuf;
use tokio::runtime::Runtime;

#[derive(Parser)]
#[command(name = "pieskieo", version, about = "Pieskieo database CLI", long_about = None)]
struct Cli {
    /// Data directory for local embedded mode
    #[arg(short, long, default_value = "data")]
    data_dir: PathBuf,

    /// Start HTTP server instead of embedded CLI ops
    #[arg(long)]
    serve: bool,

    /// Address to bind when serving
    #[arg(long, default_value = "0.0.0.0:8000")]
    listen: String,

    /// Start interactive shell (embedded mode)
    #[arg(long)]
    repl: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Insert/update a document
    PutDoc {
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        json: String,
        #[arg(long)]
        namespace: Option<String>,
        #[arg(long)]
        collection: Option<String>,
    },
    /// Query documents with exact-match filters
    QueryDoc {
        #[arg(long)]
        filter: Vec<String>, // key=val
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long)]
        namespace: Option<String>,
        #[arg(long)]
        collection: Option<String>,
    },
    /// Insert a vector
    PutVector {
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        values: Vec<f32>,
        #[arg(long)]
        namespace: Option<String>,
    },
    /// Search vectors
    SearchVector {
        #[arg(long)]
        query: Vec<f32>,
        #[arg(long, default_value_t = 10)]
        k: usize,
        #[arg(long)]
        namespace: Option<String>,
    },
    /// Start interactive shell
    Repl,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.serve {
        std::env::set_var("PIESKIEO_DATA", cli.data_dir.to_string_lossy().to_string());
        std::env::set_var("PIESKIEO_LISTEN", cli.listen);
        // hand off to server main
        let rt = Runtime::new()?;
        return rt.block_on(pieskieo_server::serve());
    }

    // embedded mode
    let db = PieskieoDb::open_with_params(&cli.data_dir, VectorParams::default())?;

    if cli.repl || matches!(cli.command, Some(Commands::Repl)) {
        run_repl(db)?;
        return Ok(());
    }

    match cli.command {
        Some(Commands::PutDoc {
            id,
            json,
            namespace,
            collection,
        }) => {
            let val: serde_json::Value = serde_json::from_str(&json)?;
            let uid = if let Some(id) = id {
                uuid::Uuid::parse_str(&id)?
            } else {
                uuid::Uuid::new_v4()
            };
            db.put_doc_ns(namespace.as_deref(), collection.as_deref(), uid, val)?;
            println!("{}", uid);
        }
        Some(Commands::QueryDoc {
            filter,
            limit,
            offset,
            namespace,
            collection,
        }) => {
            let mut map = serde_json::Map::new();
            for kv in filter {
                if let Some((k, v)) = kv.split_once('=') {
                    map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
                }
            }
            let res = db.query_docs_ns(
                namespace.as_deref(),
                collection.as_deref(),
                &map.into_iter().collect(),
                limit,
                offset,
            );
            for (id, v) in res {
                println!("{} {}", id, v);
            }
        }
        Some(Commands::PutVector {
            id,
            values,
            namespace,
        }) => {
            let uid = if let Some(id) = id {
                uuid::Uuid::parse_str(&id)?
            } else {
                uuid::Uuid::new_v4()
            };
            db.put_vector_ns(namespace.as_deref(), uid, values)?;
            println!("{}", uid);
        }
        Some(Commands::SearchVector {
            query,
            k,
            namespace,
        }) => {
            let hits = db.search_vector_metric_ns(
                namespace.as_deref(),
                &query,
                k,
                pieskieo_core::vector::VectorMetric::L2,
                None,
            )?;
            for h in hits {
                println!("{}\t{}", h.id, h.score);
            }
        }
        Some(Commands::Repl) => unreachable!(), // handled above
        None => {
            println!("No command given. Use --help for usage, or run with --repl.");
        }
    }

    Ok(())
}

fn run_repl(db: PieskieoDb) -> Result<()> {
    println!("Pieskieo shell. Commands: doc.put <json>, doc.get <uuid>, row.put <json>, row.get <uuid>, vec.put <list>, vec.search <list> [k], quit.");
    loop {
        print!("pieskieo> ");
        io::stdout().flush()?;
        let mut line = String::new();
        if io::stdin().read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "quit" || line == "exit" {
            break;
        }
        let mut parts = line.split_whitespace();
        let cmd = parts.next().unwrap_or("");
        match cmd {
            "doc.put" => {
                if let Some(json_str) = parts.next() {
                    let val: Value = serde_json::from_str(json_str)?;
                    let id = uuid::Uuid::new_v4();
                    db.put_doc(id, val)?;
                    println!("{}", id);
                } else {
                    println!("usage: doc.put {{json}}");
                }
            }
            "doc.get" => {
                if let Some(id_str) = parts.next() {
                    let id = uuid::Uuid::parse_str(id_str)?;
                    match db.get_doc(&id) {
                        Some(v) => println!("{v}"),
                        None => println!("not found"),
                    }
                } else {
                    println!("usage: doc.get <uuid>");
                }
            }
            "row.put" => {
                if let Some(json_str) = parts.next() {
                    let val: Value = serde_json::from_str(json_str)?;
                    let id = uuid::Uuid::new_v4();
                    db.put_row(id, &val)?;
                    println!("{}", id);
                } else {
                    println!("usage: row.put {{json}}");
                }
            }
            "row.get" => {
                if let Some(id_str) = parts.next() {
                    let id = uuid::Uuid::parse_str(id_str)?;
                    match db.get_row(&id) {
                        Some(v) => println!("{v}"),
                        None => println!("not found"),
                    }
                } else {
                    println!("usage: row.get <uuid>");
                }
            }
            "vec.put" => {
                if let Some(list) = parts.next() {
                    let vals: Vec<f32> = list
                        .trim_matches(&['[', ']'][..])
                        .split(',')
                        .filter_map(|s| s.trim().parse().ok())
                        .collect();
                    let id = uuid::Uuid::new_v4();
                    db.put_vector(id, vals)?;
                    println!("{}", id);
                } else {
                    println!("usage: vec.put [0.1,0.2,...]");
                }
            }
            "vec.search" => {
                if let Some(list) = parts.next() {
                    let vals: Vec<f32> = list
                        .trim_matches(&['[', ']'][..])
                        .split(',')
                        .filter_map(|s| s.trim().parse().ok())
                        .collect();
                    let k: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(5);
                    let hits = db.search_vector(&vals, k)?;
                    for h in hits {
                        println!("{}\t{}", h.id, h.score);
                    }
                } else {
                    println!("usage: vec.search [0.1,0.2,...] [k]");
                }
            }
            _ => println!("unknown command"),
        }
    }
    Ok(())
}
