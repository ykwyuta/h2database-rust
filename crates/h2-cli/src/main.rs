use std::env;
use std::fs;
use std::io::{self, BufRead, Write};
use anyhow::{Context, Result};
use h2::{Connection, ExecutionResult, Value};

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();

    let mut db_path = ":memory:".to_string();
    let mut script_file: Option<String> = None;
    let mut one_liner_command: Option<String> = None;
    let mut user_arg: Option<String> = None;
    let mut password_arg: Option<String> = None;

    let mut init_pgbench: Option<usize> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-i" | "--init-pgbench" => {
                let mut scale = 1;
                if i + 1 < args.len() && !args[i + 1].starts_with('-') {
                    if let Ok(s) = args[i + 1].parse::<usize>() {
                        scale = s;
                        i += 1;
                    }
                }
                init_pgbench = Some(scale);
            }
            "-f" | "--file" => {
                if i + 1 < args.len() {
                    script_file = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "-c" | "--command" => {
                if i + 1 < args.len() {
                    one_liner_command = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "-u" | "--user" => {
                if i + 1 < args.len() {
                    user_arg = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "-p" | "--password" => {
                if i + 1 < args.len() {
                    password_arg = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "--db" => {
                if i + 1 < args.len() {
                    db_path = args[i + 1].clone();
                    i += 1;
                }
            }
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            other => {
                if !other.starts_with('-') && db_path == ":memory:" {
                    db_path = other.to_string();
                }
            }
        }
        i += 1;
    }

    let conn = if db_path == ":memory:" {
        Connection::open_in_memory()?
    } else {
        Connection::open(&db_path)?
    };

    // ユーザー指定があれば認証実行
    if let Some(ref u) = user_arg {
        conn.authenticate(u, password_arg.as_deref(), "127.0.0.1")
            .with_context(|| format!("Authentication failed for user '{}'", u))?;
    }

    // 0. pgbench テーブル初期化 (-i / --init-pgbench [scale])
    if let Some(scale) = init_pgbench {
        init_pgbench_tables(&conn, scale)?;
        return Ok(());
    }

    // 1. ワンライナーコマンドの実行 (-c "SQL")
    if let Some(cmd) = one_liner_command {
        run_script(&conn, &cmd)?;
        return Ok(());
    }

    // 2. スクリプトファイルの実行 (-f script.sql)
    if let Some(file_path) = script_file {
        println!("Executing script file: {}", file_path);
        let content = fs::read_to_string(&file_path)
            .with_context(|| format!("Failed to read script file: {}", file_path))?;
        run_script(&conn, &content)?;
        return Ok(());
    }

    // 3. 対話型 REPL シェル
    println!("==================================================");
    println!("  h2database-rust CLI shell (v0.1.0)");
    println!("  Connecting to: {}", db_path);
    if let Some(ref u) = conn.current_user() {
        println!("  Connected as user: {}", u);
    }
    println!("  Type .help for instructions, .exit to quit");
    println!("==================================================");

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut buffer = String::new();

    loop {
        if buffer.is_empty() {
            print!("h2> ");
        } else {
            print!(" .. ");
        }
        stdout.flush()?;

        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break; // EOF
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // ドットコマンド（単行のみ）
        if buffer.is_empty() && trimmed.starts_with('.') {
            if trimmed == ".exit" || trimmed == ".quit" {
                println!("Bye!");
                break;
            } else if trimmed == ".help" {
                print_help();
            } else if trimmed == ".version" {
                println!("Storage version: {}", conn.version());
            } else if trimmed == ".user" {
                println!("Current user: {}", conn.current_user().unwrap_or_else(|| "admin (unrestricted)".to_string()));
            } else if trimmed.starts_with(".user ") {
                let u = trimmed[6..].trim();
                conn.set_current_user(Some(u));
                println!("Switched current session user to '{}'", u);
            } else if trimmed.starts_with(".read ") {
                let path = trimmed[6..].trim();
                match fs::read_to_string(path) {
                    Ok(content) => {
                        println!("Reading script from {}", path);
                        if let Err(e) = run_script(&conn, &content) {
                            eprintln!("Error executing script: {}", e);
                        }
                    }
                    Err(e) => eprintln!("Failed to read file '{}': {}", path, e),
                }
            } else {
                println!("Unknown command: {}. Type .help for available commands.", trimmed);
            }
            continue;
        }

        buffer.push_str(trimmed);
        buffer.push(' ');

        // セミコロン終端なら実行
        if trimmed.ends_with(';') {
            let stmt = buffer.trim();
            execute_single_statement(&conn, stmt);
            buffer.clear();
        }
    }

    Ok(())
}

fn print_help() {
    println!("Usage: h2-cli [OPTIONS] [DB_PATH]");
    println!();
    println!("Options:");
    println!("  -u, --user <USER>     Database user name");
    println!("  -p, --password <PASS> Database password");
    println!("  -f, --file <PATH>     Execute SQL statements from a file and exit");
    println!("  -c, --command <SQL>   Execute single or multiple SQL statements and exit");
    println!("  --db <PATH>           Path to database file (default: :memory:)");
    println!("  -h, --help            Show this help message");
    println!();
    println!("Interactive Dot Commands:");
    println!("  .help                 Show this help");
    println!("  .user                 Show or switch session user (.user or .user <NAME>)");
    println!("  .version              Show database version");
    println!("  .read <FILE>          Execute SQL script from file");
    println!("  .exit / .quit         Exit the shell");
}

/// スクリプト内の SQL 文をセミコロンで分割して順次実行
pub fn run_script(conn: &Connection, script: &str) -> Result<()> {
    let statements = split_sql_statements(script);
    for stmt in statements {
        let trimmed = stmt.trim();
        if trimmed.is_empty() {
            continue;
        }
        execute_single_statement(conn, trimmed);
    }
    Ok(())
}

fn execute_single_statement(conn: &Connection, sql: &str) {
    println!("\nh2> {}", sql);
    match conn.execute_raw(sql) {
        Ok(ExecutionResult::Query { columns, rows }) => {
            if !columns.is_empty() {
                println!("  {}", columns.join(" | "));
                let separator: Vec<String> = columns.iter().map(|c| "-".repeat(c.len().max(4))).collect();
                println!("  {}", separator.join("-+-"));
            }
            for (i, row) in rows.iter().enumerate() {
                let values_str: Vec<String> = (0..row.len())
                    .map(|idx| match row.get(idx) {
                        Some(Value::String(s)) => s.clone(),
                        Some(other) => other.to_string(),
                        None => "NULL".to_string(),
                    })
                    .collect();
                println!("  [{}] {}", i + 1, values_str.join(" | "));
            }
            println!("  ({} row(s))", rows.len());
        }
        Ok(ExecutionResult::Dml { affected_rows }) => {
            println!("  Success (affected rows: {})", affected_rows);
        }
        Ok(ExecutionResult::Ddl) => {
            println!("  Success: Command executed successfully");
        }
        Err(e) => {
            eprintln!("  Error: {}", e);
        }
    }
}

/// SQL スクリプトをセミコロン（;）で分割（文字列リテラルおよびコメントを考慮）
fn split_sql_statements(script: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_line_comment = false;

    let chars: Vec<char> = script.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let ch = chars[i];

        if in_line_comment {
            if ch == '\n' {
                in_line_comment = false;
                current.push(' ');
            }
            i += 1;
            continue;
        }

        if !in_single_quote && ch == '-' && i + 1 < len && chars[i + 1] == '-' {
            in_line_comment = true;
            i += 2;
            continue;
        }

        if ch == '\'' {
            in_single_quote = !in_single_quote;
            current.push(ch);
            i += 1;
            continue;
        }

        if ch == ';' && !in_single_quote {
            let trimmed = current.trim();
            if !trimmed.is_empty() {
                statements.push(trimmed.to_string());
            }
            current.clear();
            i += 1;
            continue;
        }

        current.push(ch);
        i += 1;
    }

    let trimmed = current.trim();
    if !trimmed.is_empty() {
        statements.push(trimmed.to_string());
    }

    statements
}

fn init_pgbench_tables(conn: &Connection, scale: usize) -> Result<()> {
    println!("Initializing pgbench tables (scale {} = {} accounts)...", scale, scale * 100_000);
    let start = std::time::Instant::now();
    let _ = conn.execute("DROP TABLE IF EXISTS pgbench_history;");
    let _ = conn.execute("DROP TABLE IF EXISTS pgbench_tellers;");
    let _ = conn.execute("DROP TABLE IF EXISTS pgbench_accounts;");
    let _ = conn.execute("DROP TABLE IF EXISTS pgbench_branches;");

    conn.execute("CREATE TABLE pgbench_branches (bid INT PRIMARY KEY, bbalance INT, filler VARCHAR(88));")?;
    conn.execute("CREATE TABLE pgbench_tellers (tid INT PRIMARY KEY, bid INT, tbalance INT, filler VARCHAR(84));")?;
    conn.execute("CREATE TABLE pgbench_accounts (aid INT PRIMARY KEY, bid INT, abalance INT, filler VARCHAR(84));")?;
    conn.execute("CREATE TABLE pgbench_history (tid INT, bid INT, aid INT, delta INT, mtime TIMESTAMP, filler VARCHAR(22));")?;

    for b in 1..=scale {
        conn.execute(&format!("INSERT INTO pgbench_branches VALUES ({}, 0, 'branch');", b))?;
    }
    for t in 1..=(scale * 10) {
        let b = ((t - 1) % scale) + 1;
        conn.execute(&format!("INSERT INTO pgbench_tellers VALUES ({}, {}, 0, 'teller');", t, b))?;
    }

    let total_accounts = scale * 100_000;
    let batch_size = 1000;
    let chunk_size = 10_000;

    let mut aid = 1;
    while aid <= total_accounts {
        let chunk_end = (aid + chunk_size - 1).min(total_accounts);
        conn.execute("BEGIN;")?;
        while aid <= chunk_end {
            let next_aid = (aid + batch_size).min(chunk_end + 1);
            let mut sql = String::with_capacity(batch_size * 40);
            sql.push_str("INSERT INTO pgbench_accounts VALUES ");
            for a in aid..next_aid {
                let bid = ((a - 1) % scale) + 1;
                use std::fmt::Write;
                let _ = write!(sql, "({},{},1000,'filler'),", a, bid);
            }
            sql.pop();
            sql.push(';');
            conn.execute(&sql)?;
            aid = next_aid;
        }
        conn.execute("COMMIT;")?;
        if (aid - 1) % 100_000 == 0 || aid > total_accounts {
            let pct = ((aid - 1) as f64 / total_accounts as f64) * 100.0;
            println!("  Inserted {} of {} accounts ({:.0}%, elapsed: {:.2?})...", aid - 1, total_accounts, pct, start.elapsed());
        }
    }

    // Checkpoint to flush to disk
    let _ = conn.execute("CHECKPOINT;");

    println!("Completed pgbench initialization ({} accounts) in {:.2?}", total_accounts, start.elapsed());
    Ok(())
}
