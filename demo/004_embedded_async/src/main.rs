use h2::{params, AsyncConnection, H2Result};

#[tokio::main]
async fn main() -> H2Result<()> {
    println!("============================================================");
    println!("  H2 Database in Rust - Asynchronous Embedded Demo (Tokio)");
    println!("============================================================");

    // 1. 非同期でデータベースを開く
    let db_path = "embedded_async.h2";
    let conn = AsyncConnection::open(db_path).await?;
    println!("[1] Opened async database at: {db_path}");

    // 2. 非同期 DDL の実行
    conn.execute(
        "CREATE TABLE IF NOT EXISTS job_queue (
            id INT PRIMARY KEY,
            job_name VARCHAR(100) NOT NULL,
            worker_id VARCHAR(50),
            status VARCHAR(20) NOT NULL,
            attempts INT NOT NULL DEFAULT 0,
            payload JSON
        )"
    ).await?;
    println!("[2] Table 'job_queue' is ready.");

    // 3. 非同期トランザクション (AsyncTransaction)
    println!("\n[3] Testing AsyncTransaction (Rollback & Commit):");
    {
        let tx = conn.transaction().await?;
        tx.execute("INSERT INTO job_queue VALUES (1, 'process-payment', NULL, 'PENDING', 0, '{\"amount\": 500}')").await?;
        println!("  Inside async tx: inserted job #1.");
        tx.rollback().await?;
        println!("  Rolled back async tx.");

        let count = conn.query("SELECT COUNT(*) FROM job_queue").await?;
        let n: i64 = count[0].get_as(0)?;
        println!("  After rollback: total jobs = {n}");
    }

    {
        let tx = conn.transaction().await?;
        tx.execute("INSERT INTO job_queue VALUES (1, 'process-payment', NULL, 'PENDING', 0, '{\"amount\": 500}')").await?;
        tx.commit().await?;
        println!("  Committed async tx for job #1.");

        let count = conn.query("SELECT COUNT(*) FROM job_queue").await?;
        let n: i64 = count[0].get_as(0)?;
        println!("  After commit: total jobs = {n}");
    }

    // 4. Tokio の複数並行タスク（Worker）からの同時非同期書き込み
    println!("\n[4] Concurrently spawning 10 worker tasks inserting jobs...");
    let mut handles = vec![];
    for worker_idx in 10..20 {
        let conn_clone = conn.clone();
        let handle = tokio::spawn(async move {
            let job_id = worker_idx;
            let worker_tag = format!("worker-tokio-{:02}", worker_idx - 10);
            conn_clone.execute_params(
                "INSERT INTO job_queue VALUES (?, ?, ?, ?, ?, ?)",
                &params![
                    job_id,
                    format!("batch-task-{}", job_id),
                    worker_tag,
                    "PROCESSING",
                    1,
                    serde_json::json!({"priority": "high", "step": 1})
                ],
            ).await.expect("Worker insert failed");
        });
        handles.push(handle);
    }

    for h in handles {
        h.await.expect("Task join failed");
    }
    println!("  All 10 concurrent worker tasks completed!");

    // 5. 非同期クエリと集約
    println!("\n[5] Querying job statistics asynchronously:");
    let stats = conn.query("SELECT status, COUNT(*) AS count FROM job_queue GROUP BY status ORDER BY count DESC").await?;
    for row in stats {
        let status: String = row.get_as(0)?;
        let count: i64 = row.get_as(1)?;
        println!("  - Status: {status} -> {count} jobs");
    }

    // 6. JSON 抽出演算子を伴う非同期クエリ
    println!("\n[6] Querying jobs with JSON extraction (payload ->> 'priority'):");
    let priority_jobs = conn.query("SELECT id, job_name, worker_id FROM job_queue WHERE payload ->> 'priority' = 'high' LIMIT 3").await?;
    for row in priority_jobs {
        let id: i32 = row.get_as(0)?;
        let name: String = row.get_as(1)?;
        let worker: String = row.get_as(2)?;
        println!("  - #{id} {name} (Assigned: {worker})");
    }

    // 7. 非同期ストレージコンパクション (Vacuum)
    println!("\n[7] Running asynchronous vacuum...");
    conn.vacuum().await?;
    println!("  Async vacuum complete! Current storage version: {}", conn.version());

    println!("\n[SUCCESS] Asynchronous embedded demo finished cleanly!");
    Ok(())
}
