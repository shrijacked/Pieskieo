use anyhow::Result;
use clap::{Parser, Subcommand};
use serde::Deserialize;
use pieskieo_core::{PieskieoDb, VectorParams};
use pieskieo_server;
use serde_json::Value;
use std::io::{self, Write};
use std::time::Instant;
use reqwest::blocking::Client;
use rustyline::DefaultEditor;
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

    /// Target server URL (enables network mode similar to psql)
    #[arg(long, env = "PIESKIEO_URL")]
    server_url: Option<String>,

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
    /// Execute a SQL/PQL statement against the server (--server-url required)
    Sql {
        #[arg(long)]
        sql: String,
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

    // network mode
    if let Some(base_url) = cli.server_url.clone() {
        let token = std::env::var("PIESKIEO_TOKEN").ok();
        let client = reqwest::blocking::Client::builder()
            .user_agent("pieskieo-cli")
            .build()?;
        if cli.repl || matches!(cli.command, Some(Commands::Repl)) || cli.command.is_none() {
            return run_net_repl(&client, &base_url, token.as_deref());
        }
        if let Some(Commands::Sql { sql }) = cli.command {
            let data = net_query_sql(&client, &base_url, token.as_deref(), &sql)?;
            println!("{}", data);
            return Ok(());
        }
        println!("In network mode (--server-url). Use --repl or Sql command.");
        return Ok(());
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
        Some(Commands::Sql { .. }) => unreachable!(), // handled in network mode
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

fn net_query_sql(
    client: &Client,
    base: &str,
    token: Option<&str>,
    sql: &str,
) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct Resp {
        ok: bool,
        data: serde_json::Value,
    }
    let mut req = client.post(format!("{}/v1/sql", base)).json(&serde_json::json!({ "sql": sql }));
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req.send()?.error_for_status()?;
    let parsed: Resp = resp.json()?;
    if !parsed.ok {
        anyhow::bail!("server returned ok=false");
    }
    Ok(serde_json::to_string_pretty(&parsed.data)?)
}

fn run_net_repl(client: &Client, base: &str, token: Option<&str>) -> Result<()> {
    let mut rl = DefaultEditor::new().ok();
    println!("Pieskieo network shell. Type SQL/PQL statements; 'quit' to exit.");
    loop {
        let prompt = "pieskieo(net)> ";
        let line = if let Some(ref mut editor) = rl {
            match editor.readline(prompt) {
                Ok(l) => {
                    let _ = editor.add_history_entry(l.as_str());
                    l
                }
                Err(_) => break,
            }
        } else {
            print!("{prompt}");
            io::stdout().flush()?;
            let mut buf = String::new();
            if io::stdin().read_line(&mut buf)? == 0 {
                break;
            }
            buf
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.eq_ignore_ascii_case("quit") || line.eq_ignore_ascii_case("exit") {
            break;
        }
        let start = Instant::now();
        match net_query_sql(client, base, token, line) {
            Ok(out) => {
                let elapsed = start.elapsed();
                println!("{out}\n({:.2?})", elapsed);
            }
            Err(e) => eprintln!("error: {e}"),
        }
    }
    Ok(())
}
