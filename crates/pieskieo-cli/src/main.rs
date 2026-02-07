use anyhow::Result;
use clap::{Parser, Subcommand};
use ctrlc;
use pieskieo_core::{PieskieoDb, SqlResult, VectorParams};
use pieskieo_server;
use reqwest::blocking::Client;
use rustyline::DefaultEditor;
use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Instant;
use tokio::runtime::Runtime;

#[derive(Parser)]
#[command(name = "pieskieo", version, about = "Pieskieo database CLI", long_about = None)]
struct Cli {
    /// Data directory for local embedded mode
    #[arg(short, long)]
    data_dir: Option<PathBuf>,

    /// Start HTTP server instead of embedded CLI ops
    #[arg(long)]
    serve: bool,

    /// Target server URL (enables network mode similar to psql)
    #[arg(long, env = "PIESKIEO_URL")]
    server_url: Option<String>,

    /// Address to bind when serving
    #[arg(long, default_value = "0.0.0.0:8000")]
    listen: String,

    /// Start interactive shell (embedded mode or network, see --server-url)
    #[arg(long)]
    repl: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Connect to a server and open an interactive PQL shell (psql-like)
    Connect {
        #[arg(short = 'H', long, default_value = "localhost")]
        host: String,
        #[arg(short = 'p', long, default_value_t = 8000)]
        port: u16,
        #[arg(short = 'U', long)]
        user: Option<String>,
        /// Prompt for password
        #[arg(short = 'W', long, action = clap::ArgAction::SetTrue)]
        prompt_password: bool,
        /// Password inline (discouraged; overrides prompt)
        #[arg(short = 'P', long)]
        password: Option<String>,
        /// Bearer token; if omitted, PIESKIEO_TOKEN env is used. If set, overrides password.
        #[arg(short = 't', long)]
        token: Option<String>,
    },
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

    /// Follow WAL from a leader and apply to a follower server
    Follow {
        /// Leader base URL (e.g. http://leader:8000)
        #[arg(long)]
        leader: String,
        /// Follower base URL (default: server_url/env PIESKIEO_URL)
        #[arg(long)]
        follower: Option<String>,
        /// Persist offset to this file (default: .pieskieo-offset)
        #[arg(long, default_value = ".pieskieo-offset")]
        offset_file: PathBuf,
        /// Poll interval seconds
        #[arg(long, default_value_t = 2)]
        interval: u64,
        /// Bearer token for leader (defaults env PIESKIEO_TOKEN)
        #[arg(long)]
        leader_token: Option<String>,
        /// Bearer token for follower (defaults env PIESKIEO_TOKEN_FOLLOWER else leader token)
        #[arg(long)]
        follower_token: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let data_dir = cli.data_dir.clone().unwrap_or_else(default_data_dir);

    if cli.serve {
        std::env::set_var("PIESKIEO_DATA", data_dir.to_string_lossy().to_string());
        std::env::set_var("PIESKIEO_LISTEN", cli.listen);
        // hand off to server main
        let rt = Runtime::new()?;
        return rt.block_on(pieskieo_server::serve());
    }

    // network mode
    if let Some(base_url) = cli.server_url.clone() {
        return run_network_mode(cli, &base_url);
    }

    // embedded mode
    let db = PieskieoDb::open_with_params(&data_dir, VectorParams::default())?;

    if cli.repl || matches!(cli.command, Some(Commands::Repl)) {
        run_repl(db)?;
        return Ok(());
    }

    match cli.command {
        Some(Commands::Connect {
            host,
            port,
            user,
            prompt_password,
            password,
            token,
        }) => {
            let base_url = format!("http://{host}:{port}");
            let token = token.or_else(|| std::env::var("PIESKIEO_TOKEN").ok());
            let mut pass = password;
            if prompt_password && pass.is_none() {
                let p = rpassword::prompt_password("Password: ")?;
                pass = Some(p);
            }
            let client = reqwest::blocking::Client::builder()
                .user_agent("pieskieo-cli")
                .build()?;
            return run_net_repl_with_prompt(
                &client,
                &base_url,
                AuthOpt {
                    bearer: token.clone(),
                    basic_user: user.clone(),
                    basic_pass: pass.clone(),
                },
                Some(user.unwrap_or_else(|| "anon".into())),
                Some(format!("{host}:{port}")),
            );
        }
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
        Some(Commands::Follow { .. }) => unreachable!(), // handled in network mode
        None => {
            println!("No command given. Use --help for usage, or run with --repl.");
        }
    }

    Ok(())
}

fn default_data_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        std::env::var("APPDATA")
            .map(|p| PathBuf::from(p).join("Pieskieo"))
            .unwrap_or_else(|_| PathBuf::from("C:\\ProgramData\\Pieskieo"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        PathBuf::from("/var/lib/pieskieo")
    }
}

fn run_network_mode(cli: Cli, base_url: &str) -> Result<()> {
    let token = std::env::var("PIESKIEO_TOKEN").ok();
    let client = reqwest::blocking::Client::builder()
        .user_agent("pieskieo-cli")
        .build()?;
    match cli.command {
        Some(Commands::Sql { sql }) => {
            let data = net_query_sql(
                &client,
                base_url,
                AuthOpt {
                    bearer: token.clone(),
                    basic_user: None,
                    basic_pass: None,
                },
                &sql,
            )?;
            println!("{}", data);
            Ok(())
        }
        Some(Commands::Follow {
            leader,
            follower,
            offset_file,
            interval,
            leader_token,
            follower_token,
        }) => {
            let follower_url = follower.unwrap_or_else(|| base_url.to_string());
            follow_replication(
                &client,
                &leader,
                &follower_url,
                offset_file,
                interval,
                leader_token.or_else(|| token.clone()),
                follower_token
                    .or_else(|| std::env::var("PIESKIEO_TOKEN_FOLLOWER").ok())
                    .or(token),
            )
        }
        Some(Commands::Repl) | None if cli.repl || cli.command.is_none() => run_net_repl(
            &client,
            base_url,
            AuthOpt {
                bearer: token,
                basic_user: None,
                basic_pass: None,
            },
        ),
        Some(Commands::PutDoc { .. })
        | Some(Commands::QueryDoc { .. })
        | Some(Commands::PutVector { .. })
        | Some(Commands::SearchVector { .. }) => {
            println!("Use the SQL or REPL commands in network mode.");
            Ok(())
        }
        _ => {
            println!("Network mode supports Sql and Repl. Use --repl or --command Sql.");
            Ok(())
        }
    }
}

fn run_repl(db: PieskieoDb) -> Result<()> {
    println!("Pieskieo embedded shell (PQL-lite + shortcuts). Type `help` for commands or enter raw PQL SELECT/INSERT/UPDATE/DELETE. quit/exit to leave.");
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
        if run_repl_command(&db, line.to_string())? {
            continue;
        }
    }
    Ok(())
}

fn run_repl_command(db: &PieskieoDb, line: String) -> Result<bool> {
    let trimmed = line.trim();
    if trimmed.eq_ignore_ascii_case("help") {
        println!("Commands:\n  doc.put <json>\n  doc.get <uuid>\n  row.put <json>\n  row.get <uuid>\n  vec.put [0.1,0.2,...]\n  vec.search [0.1,0.2,...] [k]\nOr enter raw PQL like: SELECT * FROM default.people WHERE age > 20 LIMIT 5;");
        return Ok(true);
    }
    if trimmed.is_empty() {
        return Ok(true);
    }
    if trimmed.ends_with(';')
        || trimmed.to_uppercase().starts_with("SELECT")
        || trimmed.to_uppercase().starts_with("INSERT")
        || trimmed.to_uppercase().starts_with("UPDATE")
        || trimmed.to_uppercase().starts_with("DELETE")
    {
        match db.query_sql(trimmed.trim_end_matches(';'))? {
            SqlResult::Select(rows) => {
                for (id, v) in rows {
                    println!("{}\t{}", id, v);
                }
            }
            SqlResult::Insert { ids } => {
                for id in ids {
                    println!("{id}");
                }
            }
            SqlResult::Update { affected } | SqlResult::Delete { affected } => {
                println!("affected: {affected}");
            }
        }
        return Ok(true);
    }
    let mut parts = trimmed.split_whitespace();
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
        _ => println!("unknown command; type help"),
    }
    Ok(true)
}

#[derive(Clone, Default)]
struct AuthOpt {
    bearer: Option<String>,
    basic_user: Option<String>,
    basic_pass: Option<String>,
}

fn net_query_sql(client: &Client, base: &str, auth: AuthOpt, sql: &str) -> Result<String> {
    let (out, _) = net_query_sql_with_status(client, base, auth, sql)?;
    Ok(out)
}

fn net_query_sql_with_status(
    client: &Client,
    base: &str,
    auth: AuthOpt,
    sql: &str,
) -> Result<(String, Option<reqwest::StatusCode>)> {
    #[derive(Deserialize)]
    struct Resp {
        ok: bool,
        data: serde_json::Value,
    }
    let mut req = client
        .post(format!("{}/v1/sql", base))
        .json(&serde_json::json!({ "sql": sql }));
    if let Some(t) = auth.bearer {
        req = req.bearer_auth(t);
    } else if let Some(user) = auth.basic_user {
        req = req.basic_auth(user, auth.basic_pass);
    }
    let resp = req.send()?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("HTTP status {} for /v1/sql", status);
    }
    let parsed: Resp = resp.json()?;
    if !parsed.ok {
        anyhow::bail!("server returned ok=false");
    }
    Ok((serde_json::to_string_pretty(&parsed.data)?, Some(status)))
}

fn run_net_repl(client: &Client, base: &str, auth: AuthOpt) -> Result<()> {
    run_net_repl_with_prompt(client, base, auth, None, None)
}

fn run_net_repl_with_prompt(
    client: &Client,
    base: &str,
    auth: AuthOpt,
    user: Option<String>,
    target: Option<String>,
) -> Result<()> {
    let mut rl = DefaultEditor::new().ok();
    let target_disp = target.unwrap_or_else(|| base.to_string());
    let user_disp = user.unwrap_or_else(|| "anon".into());
    println!(
        "Pieskieo network shell connected to {target_disp} as {user_disp}. Type SQL/PQL statements; 'quit' to exit."
    );
    // Basic-auth validation with retry prompt
    if auth.basic_user.is_some() {
        let mut attempt = 0;
        let mut auth_mut = auth.clone();
        loop {
            match net_query_sql_with_status(client, base, auth_mut.clone(), "SELECT 1") {
                Ok(_) => break,
                Err(e) => {
                    attempt += 1;
                    if attempt >= 3 {
                        return Err(anyhow::anyhow!(
                            "authentication failed after 3 attempts: {e}"
                        ));
                    }
                    println!("Authentication failed ({e}). Please re-enter password.");
                    let p = rpassword::prompt_password("Password: ")?;
                    auth_mut.basic_pass = Some(p);
                }
            }
        }
    }

    let mut buffer = String::new();
    loop {
        let prompt = if buffer.is_empty() {
            "pieskieo(net)> "
        } else {
            "....> "
        };
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
        buffer.push_str(line);
        buffer.push(' ');
        let complete = line.trim_end().ends_with(';');
        if !complete {
            continue;
        }
        let stmt = buffer.trim();
        if stmt.is_empty() {
            buffer.clear();
            continue;
        }
        let start = Instant::now();
        match net_query_sql(client, base, auth.clone(), stmt) {
            Ok(out) => {
                let elapsed = start.elapsed();
                println!("{out}\n({:.2?})", elapsed);
            }
            Err(e) => eprintln!("error: {e}"),
        }
        buffer.clear();
    }
    Ok(())
}

fn follow_replication(
    client: &Client,
    leader: &str,
    follower: &str,
    offset_file: PathBuf,
    interval: u64,
    leader_token: Option<String>,
    follower_token: Option<String>,
) -> Result<()> {
    let mut offset = read_offset(&offset_file)?;
    println!(
        "following WAL from {} -> {} starting at offset {} (ctrl+c to stop)",
        leader, follower, offset
    );
    let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    {
        let running = running.clone();
        ctrlc::set_handler(move || {
            running.store(false, std::sync::atomic::Ordering::SeqCst);
        })
        .ok();
    }
    while running.load(std::sync::atomic::Ordering::SeqCst) {
        let url = format!("{}/v1/replica/wal?since={}", leader, offset);
        let mut req = client.get(&url);
        if let Some(tok) = &leader_token {
            req = req.bearer_auth(tok);
        }
        let resp = req.send()?;
        if !resp.status().is_success() {
            eprintln!("leader {} responded {}", leader, resp.status());
            std::thread::sleep(std::time::Duration::from_secs(interval));
            continue;
        }
        let val: serde_json::Value = resp.json()?;
        let slices = val["data"]["slices"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut max_end = offset;
        for slice in slices {
            let end = slice["end_offset"].as_u64().unwrap_or(offset);
            let records = slice["records"].as_array().cloned().unwrap_or_default();
            if records.is_empty() {
                max_end = max_end.max(end);
                continue;
            }
            let apply_url = format!("{}/v1/replica/apply", follower);
            let mut req = client
                .post(&apply_url)
                .json(&serde_json::json!({ "records": records }));
            if let Some(tok) = &follower_token {
                req = req.bearer_auth(tok);
            }
            let apply_resp = req.send()?;
            if !apply_resp.status().is_success() {
                eprintln!("follower apply failed {}", apply_resp.status());
                std::thread::sleep(std::time::Duration::from_secs(interval));
                continue;
            }
            max_end = max_end.max(end);
        }
        if max_end > offset {
            offset = max_end;
            write_offset(&offset_file, offset)?;
        } else {
            std::thread::sleep(std::time::Duration::from_secs(interval));
        }
    }
    println!("stopped at offset {}", offset);
    Ok(())
}

fn read_offset(path: &PathBuf) -> Result<u64> {
    if let Ok(s) = fs::read_to_string(path) {
        if let Ok(v) = s.trim().parse::<u64>() {
            return Ok(v);
        }
    }
    Ok(0)
}

fn write_offset(path: &PathBuf, offset: u64) -> Result<()> {
    fs::write(path, offset.to_string())?;
    Ok(())
}
