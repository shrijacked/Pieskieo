use anyhow::Result;
use clap::{Parser, Subcommand};
use ctrlc;
use reqwest::blocking::Client;
use rustyline::DefaultEditor;
use serde::Deserialize;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Instant;

#[derive(Parser)]
#[command(name = "pieskieo", version, about = "Pieskieo database CLI (network)", long_about = None)]
struct Cli {
    /// Target server URL (defaults to http://127.0.0.1:8000 or PIESKIEO_URL)
    #[arg(long, env = "PIESKIEO_URL")]
    server_url: Option<String>,

    /// Start interactive shell (network)
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
    /// Execute a SQL/PQL statement against the server
    Sql {
        #[arg(long)]
        sql: String,
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
    let base_url = cli
        .server_url
        .clone()
        .or_else(|| std::env::var("PIESKIEO_URL").ok())
        .unwrap_or_else(|| "http://127.0.0.1:8000".into());

    run_network_mode(cli, &base_url)
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
        _ => {
            println!("Network mode supports Repl, Connect, Sql, Follow.");
            Ok(())
        }
    }
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
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        anyhow::bail!("invalid username/password");
    }
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
                    println!("Invalid username or password. Please re-enter password.");
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
