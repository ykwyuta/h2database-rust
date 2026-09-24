# demo/003 Rustでの組み込み利用（同期） (Synchronous Embedded Rust API)

本デモでは、Rust アプリケーション内に `h2` クレートを直接組み込み、**SQLite（rusqlite）のような手軽さ**で同期的に高速データベース操作を行う方法を解説します。

---

## 🌟 特長

- **外部プロセス不要**: ライブラリとしてアプリケーションプロセス内に直接リンク。
- **高機能型システム**: `UUID`, `Decimal`（金融計算）, `JSON`, `DateTime` を標準サポート。
- **型安全な結果抽出**: `Row::get_as::<T>(index)` により、コンパイル時・実行時に安全な型変換。
- **MVCC トランザクション**: スナップショット分離、RAII による自動ロールバック。
- **日本語全文検索**: 外部拡張なしで N-Gram / 形態素解析 FTS を標準内蔵。

---

## 🚀 実行方法

リポジトリルートから以下のコマンドを実行します。

```bash
cargo run -p demo-embedded-sync
```

または本ディレクトリ内で:
```bash
cd demo/003_embedded_sync
cargo run
```

---

## 📖 実装コードのハイライト

ソースコード: [`src/main.rs`](./src/main.rs)

### 1. 接続のオープン
```rust
use h2::{Connection, H2Result};

let conn = Connection::open("embedded_sync.h2")?;
// インメモリで実行したい場合は:
// let conn = Connection::open_in_memory()?;
```

### 2. パラメータ付き DML
プレースホルダー（`?` や `$1`）と `params!` マクロを組み合わせて SQL インジェクションを防止します。
```rust
conn.execute_params(
    "INSERT INTO articles VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    &params![
        1,
        uid,
        "Rustで始めるデータベースプログラミング",
        1500,
        Decimal::from_str("29.80").unwrap(),
        serde_json::json!(["Rust", "Database"]),
        "本文...",
        true
    ],
)?;
```

### 3. 型安全なクエリ結果の走査 (`Row::get_as`)
```rust
let rows = conn.query("SELECT id, title, views, price, published FROM articles ORDER BY views DESC")?;
for row in rows {
    let id: i32 = row.get_as(0)?;
    let title: String = row.get_as(1)?;
    let views: i32 = row.get_as(2)?;
    let price: Decimal = row.get_as(3)?;
    let is_pub: bool = row.get_as(4)?;
    println!("Article #{id}: {title} (${price})");
}
```

### 4. トランザクション (スナップショット分離)
```rust
let tx = conn.transaction()?;
tx.execute("UPDATE articles SET views = views + 50 WHERE id = 1")?;
tx.commit()?; // commit() を呼ばずにスコープを抜けると自動ロールバック
```

### 5. 日本語全文検索 (FTS)
```rust
let fts_rows = conn.query("SELECT id, title FROM articles WHERE FT_SEARCH(content, 'スナップショット')")?;
```

### 6. ストレージのコンパクション (Vacuum)
```rust
conn.vacuum()?;
```
