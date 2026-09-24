use std::env;
use std::io::{self, BufRead, Write};
use anyhow::Result;
use h2::{Connection, Value};

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    let db_path = args.get(1).map(|s| s.as_str()).unwrap_or(":memory:");

    println!("==================================================");
    println!(" h2database-rust CLI shell (v0.1.0)");
    println!(" Connecting to: {}", db_path);
    println!(" Type .help for instructions, .exit to quit");
    println!("==================================================");

    let conn = if db_path == ":memory:" {
        Connection::open_in_memory()?
    } else {
        Connection::open(db_path)?
    };

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        print!("h2> ");
        stdout.flush()?;

        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break; // EOF
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if trimmed == ".exit" || trimmed == ".quit" {
            println!("Bye!");
            break;
        }

        if trimmed == ".help" {
            println!("Commands:");
            println!("  .help         Show this help");
            println!("  .version      Show database version");
            println!("  .exit / .quit Exit the shell");
            println!("  <SQL>         Execute SQL statement (e.g. CREATE TABLE, INSERT, SELECT)");
            continue;
        }

        if trimmed == ".version" {
            println!("Storage version: {}", conn.version());
            continue;
        }

        // クエリ判定
        let upper = trimmed.to_uppercase();
        if upper.starts_with("SELECT") {
            match conn.query(trimmed) {
                Ok(rows) => {
                    println!("Result: {} row(s)", rows.len());
                    for (i, row) in rows.iter().enumerate() {
                        let values_str: Vec<String> = (0..row.len())
                            .map(|idx| match row.get(idx) {
                                Some(Value::String(s)) => s.clone(),
                                Some(other) => other.to_string(),
                                None => "NULL".to_string(),
                            })
                            .collect();
                        println!(" [{}] {}", i + 1, values_str.join(" | "));
                    }
                }
                Err(e) => eprintln!("Error: {}", e),
            }
        } else {
            match conn.execute(trimmed) {
                Ok(affected) => println!("Success (affected rows: {})", affected),
                Err(e) => eprintln!("Error: {}", e),
            }
        }
    }

    Ok(())
}
