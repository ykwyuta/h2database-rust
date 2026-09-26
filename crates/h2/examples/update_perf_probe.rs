//! Reproducible local UPDATE probe. Run with:
//! PROBE_ROWS=10000 PROBE_ITERS=200 PROBE_SYNC=1 cargo run --release -p h2 --example update_perf_probe

use std::error::Error;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use h2::{ExecutionResult, MVStore, SQLEngine, Value};
use h2_server::PgServer;

#[derive(Clone)]
struct SimpleRng(u64);

impl SimpleRng {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 { 0x853c49e6748fea9b } else { seed })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn gen_range(&mut self, min: usize, max: usize) -> usize {
        if min >= max {
            return min;
        }
        min + (self.next_u64() as usize % (max - min + 1))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Workload {
    Select,
    HotUpdate,
    DistinctUpdate,
    RandomUpdate,
    SkewedUpdate,
    RangeUpdate,
    OltpMix,
}

impl Workload {
    fn name(self) -> &'static str {
        match self {
            Self::Select => "point_select",
            Self::HotUpdate => "hot_update",
            Self::DistinctUpdate => "distinct_update",
            Self::RandomUpdate => "random_update",
            Self::SkewedUpdate => "skewed_update",
            Self::RangeUpdate => "range_update",
            Self::OltpMix => "oltp_mix",
        }
    }

    fn generate_sqls(self, worker: usize, rows: usize, rng: &mut SimpleRng) -> Vec<String> {
        match self {
            Self::Select => vec!["SELECT abalance FROM probe_accounts WHERE aid = 1".to_string()],
            Self::HotUpdate => {
                vec!["UPDATE probe_accounts SET abalance = abalance + 1 WHERE aid = 1".to_string()]
            }
            Self::DistinctUpdate => vec![format!(
                "UPDATE probe_accounts SET abalance = abalance + 1 WHERE aid = {}",
                (worker % rows) + 1
            )],
            Self::RandomUpdate => {
                let aid = rng.gen_range(1, rows);
                vec![format!(
                    "UPDATE probe_accounts SET abalance = abalance + 1 WHERE aid = {}",
                    aid
                )]
            }
            Self::SkewedUpdate => {
                // 80/20 Pareto: 80% updates hit first 20% of accounts
                let hot_limit = (rows / 5).max(1);
                let aid = if rng.gen_range(1, 100) <= 80 {
                    rng.gen_range(1, hot_limit)
                } else {
                    rng.gen_range(hot_limit + 1, rows)
                };
                vec![format!(
                    "UPDATE probe_accounts SET abalance = abalance + 1 WHERE aid = {}",
                    aid
                )]
            }
            Self::RangeUpdate => {
                let max_start = rows.saturating_sub(10).max(1);
                let start = rng.gen_range(1, max_start);
                vec![format!(
                    "UPDATE probe_accounts SET abalance = abalance + 1 WHERE aid BETWEEN {} AND {}",
                    start,
                    start + 9
                )]
            }
            Self::OltpMix => {
                // 4 random point selects + 1 random point update
                let a1 = rng.gen_range(1, rows);
                let a2 = rng.gen_range(1, rows);
                let a3 = rng.gen_range(1, rows);
                let a4 = rng.gen_range(1, rows);
                let a_upd = rng.gen_range(1, rows);
                vec![
                    format!("SELECT abalance FROM probe_accounts WHERE aid = {}", a1),
                    format!("SELECT abalance FROM probe_accounts WHERE aid = {}", a2),
                    format!("SELECT abalance FROM probe_accounts WHERE aid = {}", a3),
                    format!("SELECT abalance FROM probe_accounts WHERE aid = {}", a4),
                    format!("UPDATE probe_accounts SET abalance = abalance + 1 WHERE aid = {}", a_upd),
                ]
            }
        }
    }
}

struct Trial {
    elapsed: Duration,
    latencies_us: Vec<u64>,
    errors: Vec<String>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let rows = env_usize("PROBE_ROWS", 10_000);
    let iterations = env_usize("PROBE_ITERS", 200);
    let sync = std::env::var("PROBE_SYNC").map_or(true, |v| v != "0");
    let pgwire = std::env::var("PROBE_PGWIRE").is_ok_and(|v| v == "1");
    let concurrent_checkpoint = std::env::var("PROBE_CHECKPOINT").is_ok_and(|v| v == "1");
    let workloads_filter = std::env::var("PROBE_WORKLOADS").ok();

    if rows < 8 || iterations == 0 {
        return Err("PROBE_ROWS must be at least 8 and PROBE_ITERS must be positive".into());
    }

    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("probe.h2");
    let store = Arc::new(MVStore::open(&db_path)?);
    let engine = Arc::new(SQLEngine::new(Arc::clone(&store))?);
    engine.execute(
        "CREATE TABLE probe_accounts (aid INT PRIMARY KEY, abalance INT, filler VARCHAR)",
    )?;

    let setup_started = Instant::now();
    for first in (1..=rows).step_by(100) {
        let last = (first + 99).min(rows);
        let values = (first..=last)
            .map(|id| format!("({}, 1000, 'filler')", id))
            .collect::<Vec<_>>()
            .join(",");
        engine.execute(&format!("INSERT INTO probe_accounts VALUES {}", values))?;
    }
    store.sync()?;
    let setup_ms = setup_started.elapsed().as_secs_f64() * 1000.0;
    let base_size = fs::metadata(&db_path)?.len();
    store.set_sync_on_commit(sync);

    println!(
        "CONFIG rows={} iterations_per_client={} sync_on_commit={} logical_cpus={} setup_ms={:.2} base_file_bytes={}",
        rows,
        iterations,
        sync,
        thread::available_parallelism()?.get(),
        setup_ms,
        base_size
    );
    println!("workload,clients,successes,errors,tps,p50_ms,p95_ms,p99_ms,mean_engine_ms,row_lock_ms,tree_lock_ms,commit_lock_ms,wal_lock_ms,wal_write_ms,wal_sync_ms,wal_durable_wait_ms,point_gets_per_call,scan_entries_per_call");

    let all_scenarios: Vec<(Workload, usize)> = vec![
        (Workload::Select, 1),
        (Workload::Select, 8),
        (Workload::HotUpdate, 1),
        (Workload::HotUpdate, 4),
        (Workload::HotUpdate, 8),
        (Workload::HotUpdate, 16),
        (Workload::DistinctUpdate, 4),
        (Workload::DistinctUpdate, 8),
        (Workload::DistinctUpdate, 16),
        (Workload::RandomUpdate, 1),
        (Workload::RandomUpdate, 4),
        (Workload::RandomUpdate, 8),
        (Workload::RandomUpdate, 16),
        (Workload::SkewedUpdate, 4),
        (Workload::SkewedUpdate, 8),
        (Workload::SkewedUpdate, 16),
        (Workload::RangeUpdate, 1),
        (Workload::RangeUpdate, 4),
        (Workload::RangeUpdate, 8),
        (Workload::OltpMix, 1),
        (Workload::OltpMix, 4),
        (Workload::OltpMix, 8),
        (Workload::OltpMix, 16),
    ];

    let scenarios: Vec<(Workload, usize)> = if let Some(ref filter) = workloads_filter {
        let selected: Vec<&str> = filter.split(',').map(str::trim).collect();
        all_scenarios
            .into_iter()
            .filter(|(w, _)| selected.contains(&w.name()) || selected.contains(&"all"))
            .collect()
    } else {
        all_scenarios
    };

    for &(workload, clients) in &scenarios {
        let mut warmup_rng = SimpleRng::new(42);
        for worker in 0..clients {
            let sqls = workload.generate_sqls(worker, rows, &mut warmup_rng);
            for sql in &sqls {
                for _ in 0..3 {
                    let _ = engine.execute(sql);
                }
            }
        }
        engine.execute("RESET QUERY STATS")?;
        let trial = run_trial(Arc::clone(&engine), workload, clients, iterations, rows);
        let stats = engine.execute("SHOW QUERY STATS")?;
        print_trial(workload.name(), clients, &trial, stats);
    }

    if pgwire {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let server =
            runtime.block_on(PgServer::bind("127.0.0.1:0".parse()?, Arc::clone(&engine)))?;
        let addr = server.local_addr()?;
        runtime.spawn(server.run());
        println!("PGWIRE address={}", addr);
        for &(workload, clients) in &scenarios {
            engine.execute("RESET QUERY STATS")?;
            let trial = run_pgwire_trial(addr, workload, clients, iterations, rows);
            let stats = engine.execute("SHOW QUERY STATS")?;
            print_trial(
                &format!("pgwire_{}", workload.name()),
                clients,
                &trial,
                stats,
            );
        }
        runtime.shutdown_timeout(Duration::from_secs(1));
    }

    if concurrent_checkpoint {
        engine.execute("RESET QUERY STATS")?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_worker = Arc::clone(&stop);
        let checkpoint_store = Arc::clone(&store);
        let checkpointer = thread::spawn(move || {
            let mut durations = Vec::new();
            while !stop_worker.load(Ordering::Relaxed) {
                let started = Instant::now();
                checkpoint_store.sync().expect("checkpoint failed");
                durations.push(started.elapsed());
                thread::sleep(Duration::from_secs(1));
            }
            durations
        });
        let trial = run_trial(
            Arc::clone(&engine),
            Workload::HotUpdate,
            4,
            iterations.max(1000),
            rows,
        );
        stop.store(true, Ordering::Relaxed);
        let checkpoints = checkpointer.join().expect("checkpointer panicked");
        let stats = engine.execute("SHOW QUERY STATS")?;
        print_trial("hot_update_with_checkpoint", 4, &trial, stats);
        println!(
            "CONCURRENT_CHECKPOINT count={} max_ms={:.3} file_bytes={}",
            checkpoints.len(),
            checkpoints
                .iter()
                .map(Duration::as_secs_f64)
                .fold(0.0, f64::max)
                * 1000.0,
            fs::metadata(&db_path)?.len()
        );
    }

    let pre_checkpoint_size = fs::metadata(&db_path)?.len();
    let checkpoint_started = Instant::now();
    store.sync()?;
    println!(
        "CHECKPOINT elapsed_ms={:.3} file_bytes_before={} file_bytes_after={}",
        checkpoint_started.elapsed().as_secs_f64() * 1000.0,
        pre_checkpoint_size,
        fs::metadata(&db_path)?.len()
    );
    Ok(())
}

fn run_trial(
    engine: Arc<SQLEngine>,
    workload: Workload,
    clients: usize,
    iterations: usize,
    rows: usize,
) -> Trial {
    let barrier = Arc::new(Barrier::new(clients + 1));
    let mut handles = Vec::new();
    for worker in 0..clients {
        let engine = Arc::clone(&engine);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            let mut rng = SimpleRng::new(1000 + worker as u64 * 7919);
            let mut latencies = Vec::with_capacity(iterations);
            let mut errors = Vec::new();
            barrier.wait();
            for _ in 0..iterations {
                let sqls = workload.generate_sqls(worker, rows, &mut rng);
                let started = Instant::now();
                let mut iter_err = None;
                for sql in &sqls {
                    if let Err(error) = engine.execute(sql) {
                        iter_err = Some(error.to_string());
                        break;
                    }
                }
                latencies.push(started.elapsed().as_micros().min(u64::MAX as u128) as u64);
                if let Some(error) = iter_err {
                    errors.push(error);
                }
            }
            (latencies, errors)
        }));
    }
    barrier.wait();
    let started = Instant::now();
    let mut latencies_us = Vec::with_capacity(clients * iterations);
    let mut errors = Vec::new();
    for handle in handles {
        let (latencies, worker_errors) = handle.join().expect("probe worker panicked");
        latencies_us.extend(latencies);
        errors.extend(worker_errors);
    }
    latencies_us.sort_unstable();
    Trial {
        elapsed: started.elapsed(),
        latencies_us,
        errors,
    }
}

fn print_trial(label: &str, clients: usize, trial: &Trial, stats: ExecutionResult) {
    let successes = trial.latencies_us.len() - trial.errors.len();
    let tps = successes as f64 / trial.elapsed.as_secs_f64();
    let (columns, rows) = match stats {
        ExecutionResult::Query { columns, rows } => (columns, rows),
        _ => panic!("SHOW QUERY STATS did not return rows"),
    };
    let prefix = if label.contains("select") && !label.contains("oltp_mix") {
        "SELECT"
    } else {
        "UPDATE"
    };
    let row = rows
        .iter()
        .find(|row| matches!(&row.values[0], Value::String(query) if query.starts_with(prefix)));
    let get = |name: &str| -> f64 {
        row.and_then(|row| {
            columns
                .iter()
                .position(|column| column == name)
                .map(|index| &row.values[index])
        })
        .map_or(0.0, |value| match value {
            Value::Double(value) => *value,
            Value::BigInt(value) => *value as f64,
            _ => 0.0,
        })
    };
    let calls = get("calls").max(1.0);
    println!(
        "{},{},{},{},{:.2},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.2},{:.2}",
        label,
        clients,
        successes,
        trial.errors.len(),
        tps,
        percentile(&trial.latencies_us, 0.50),
        percentile(&trial.latencies_us, 0.95),
        percentile(&trial.latencies_us, 0.99),
        get("mean_ms"),
        get("lock_wait_ms") / calls,
        get("tree_lock_wait_ms") / calls,
        get("commit_lock_wait_ms") / calls,
        get("wal_lock_wait_ms") / calls,
        get("wal_write_ms") / calls,
        get("wal_sync_ms") / calls,
        get("wal_durable_wait_ms") / calls,
        get("point_gets") / calls,
        get("scan_entries") / calls
    );
    if let Some(error) = trial.errors.first() {
        eprintln!("FIRST_ERROR {}", error);
    }
}

fn percentile(sorted_us: &[u64], fraction: f64) -> f64 {
    if sorted_us.is_empty() {
        return 0.0;
    }
    let index = ((sorted_us.len() - 1) as f64 * fraction).ceil() as usize;
    sorted_us[index] as f64 / 1000.0
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn run_pgwire_trial(
    addr: SocketAddr,
    workload: Workload,
    clients: usize,
    iterations: usize,
    rows: usize,
) -> Trial {
    let barrier = Arc::new(Barrier::new(clients + 1));
    let mut handles = Vec::new();
    for worker in 0..clients {
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            let mut rng = SimpleRng::new(1000 + worker as u64 * 7919);
            let mut stream = connect_pgwire(addr).expect("PGWire connection failed");
            for _ in 0..5 {
                let sqls = workload.generate_sqls(worker, rows, &mut rng);
                for sql in &sqls {
                    pgwire_query(&mut stream, sql).expect("PGWire warmup failed");
                }
            }
            let mut latencies = Vec::with_capacity(iterations);
            let mut errors = Vec::new();
            barrier.wait();
            for _ in 0..iterations {
                let sqls = workload.generate_sqls(worker, rows, &mut rng);
                let started = Instant::now();
                let mut iter_err = None;
                for sql in &sqls {
                    if let Err(error) = pgwire_query(&mut stream, sql) {
                        iter_err = Some(error.to_string());
                        break;
                    }
                }
                latencies.push(started.elapsed().as_micros().min(u64::MAX as u128) as u64);
                if let Some(error) = iter_err {
                    errors.push(error);
                }
            }
            (latencies, errors)
        }));
    }
    barrier.wait();
    let started = Instant::now();
    let mut latencies_us = Vec::with_capacity(clients * iterations);
    let mut errors = Vec::new();
    for handle in handles {
        let (latencies, worker_errors) = handle.join().expect("PGWire worker panicked");
        latencies_us.extend(latencies);
        errors.extend(worker_errors);
    }
    latencies_us.sort_unstable();
    Trial {
        elapsed: started.elapsed(),
        latencies_us,
        errors,
    }
}

fn connect_pgwire(addr: SocketAddr) -> std::io::Result<TcpStream> {
    let mut stream = TcpStream::connect(addr)?;
    stream.set_nodelay(true)?;
    let params = b"user\0postgres\0database\0probe\0\0";
    stream.write_all(&(8u32 + params.len() as u32).to_be_bytes())?;
    stream.write_all(&196608u32.to_be_bytes())?;
    stream.write_all(params)?;
    read_until_ready(&mut stream)?;
    Ok(stream)
}

fn pgwire_query(stream: &mut TcpStream, sql: &str) -> std::io::Result<()> {
    stream.write_all(b"Q")?;
    stream.write_all(&(sql.len() as u32 + 5).to_be_bytes())?;
    stream.write_all(sql.as_bytes())?;
    stream.write_all(&[0])?;
    read_until_ready(stream)
}

fn read_until_ready(stream: &mut TcpStream) -> std::io::Result<()> {
    let mut error = None;
    loop {
        let mut message_type = [0u8; 1];
        let mut length = [0u8; 4];
        stream.read_exact(&mut message_type)?;
        stream.read_exact(&mut length)?;
        let payload_len = u32::from_be_bytes(length).saturating_sub(4) as usize;
        let mut payload = vec![0u8; payload_len];
        stream.read_exact(&mut payload)?;
        if message_type[0] == b'E' {
            error = Some(String::from_utf8_lossy(&payload).into_owned());
        }
        if message_type[0] == b'Z' {
            return match error {
                Some(message) => Err(std::io::Error::other(message)),
                None => Ok(()),
            };
        }
    }
}
