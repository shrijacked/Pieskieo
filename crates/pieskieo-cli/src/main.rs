use anyhow::Result;
use clap::{Parser, Subcommand};
use pieskieo_core::{PieskieoDb, VectorParams};
use pieskieo_server;
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
        None => {
            println!("No command given. Use --help for usage.");
        }
    }

    Ok(())
}
