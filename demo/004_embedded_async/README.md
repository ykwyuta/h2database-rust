# demo/004 Rustでの組み込み利用（非同期） (Asynchronous Embedded Rust API with Tokio)

本デモでは、**Tokio 非同期ランタイム**を活用した `h2` クレートの非同期 API（`AsyncConnection`, `AsyncTransaction`）の使い方を解説します。

---

## 🌟 特長

- **イベントループをブロックしない**: 重たいファイル I/O や CoW B-Tree の探索・書き込みを Tokio のブロッキングスレッドプールで適切に非同期実行。
- **高スケーラビリティ**: Axum, Actix-web, Tonic (gRPC) などの非同期 Web/マイクロサービスに最適。
- **非同期トランザクション (`AsyncTransaction`)**: `tx.execute().await`, `tx.commit().await`, `tx.rollback().await` を完備。
- **スレッドセーフ（Send + Sync + Clone）**: コネクションのクローンを多数の `tokio::spawn` タスクへ配布して同時並行実行可能。

---

## 🚀 実行方法

リポジトリルートから以下のコマンドを実行します。

```bash
cargo run -p demo-embedded-async
```

または本ディレクトリ内で:
```bash
cd demo/004_embedded_async
cargo run
```

---

## 📖 実装コードのハイライト

ソースコード: [`src/main.rs`](./src/main.rs)

### 1. 非同期接続のオープン
```rust
use h2::{AsyncConnection, H2Result};

let conn = AsyncConnection::open("embedded_async.h2").await?;
```

### 2. 非同期トランザクション
```rust
let tx = conn.transaction().await?;
tx.execute("INSERT INTO job_queue VALUES (1, 'task', 'PENDING')").await?;
tx.commit().await?; // または tx.rollback().await?
```

### 3. Tokio タスクからの並行書き込み
```rust
let mut handles = vec![];
for i in 0..10 {
    let conn_clone = conn.clone();
    handles.push(tokio::spawn(async move {
        conn_clone.execute_params(
            "INSERT INTO job_queue VALUES (?, ?)",
            &params![i, format!("job-{}", i)],
        ).await.unwrap();
    }));
}
for h in handles {
    h.await.unwrap();
}
```

### 4. 非同期ストレージコンパクション
```rust
conn.vacuum().await?;
```

---

## 🌐 Web フレームワーク (Axum) での活用例

`AsyncConnection` は `Clone` を実装しているため、Axum の共有 State としてそのまま登録できます。

```rust
use axum::{extract::State, routing::get, Json, Router};
use h2::AsyncConnection;

#[derive(Clone)]
struct AppState {
    db: AsyncConnection,
}

async fn get_jobs(State(state): State<AppState>) -> Json<Vec<String>> {
    let rows = state.db.query("SELECT job_name FROM job_queue").await.unwrap();
    let jobs = rows.into_iter().map(|r| r.get_as::<String>(0).unwrap()).collect();
    Json(jobs)
}

pub fn app(db: AsyncConnection) -> Router {
    Router::new().route("/jobs", get(get_jobs)).with_state(AppState { db })
}
```
