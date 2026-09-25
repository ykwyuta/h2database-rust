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

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
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
    println!("  -f, --file <PATH>     Execute SQL statements from a file and exit");
    println!("  -c, --command <SQL>   Execute single or multiple SQL statements and exit");
    println!("  --db <PATH>           Path to database file (default: :memory:)");
    println!("  -h, --help            Show this help message");
    println!();
    println!("Interactive Dot Commands:");
    println!("  .help                 Show this help");
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
