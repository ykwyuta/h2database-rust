use std::sync::Arc;
use arrow::array::*;
use chrono::NaiveDate;

use h2_mvstore::buffer_pool::slotted_page::{SlottedPage, PAGE_SIZE};
use h2_mvstore::MVStore;
use h2_sql::catalog::ColumnDef;
use h2_sql::row::Row;
use h2_sql::vectorized::*;
use h2_sql::{ExecutionResult, SQLEngine};
use h2_types::{DataType as H2DataType, Value};

#[test]
fn test_arrow_type_mapping_and_roundtrip() {
    let columns = vec![
        ColumnDef::new("id".to_string(), H2DataType::Integer, false, true),
        ColumnDef::new("name".to_string(), H2DataType::VarChar(None), true, false),
        ColumnDef::new("salary".to_string(), H2DataType::Double, true, false),
        ColumnDef::new("is_active".to_string(), H2DataType::Boolean, true, false),
        ColumnDef::new("birthday".to_string(), H2DataType::Date, true, false),
    ];

    let schema = create_arrow_schema(&columns);
    assert_eq!(schema.fields().len(), 5);

    let rows = vec![
        Row::new(vec![
            Value::Integer(1),
            Value::String("Alice".to_string()),
            Value::Double(75000.5),
            Value::Boolean(true),
            Value::Date(NaiveDate::from_ymd_opt(1990, 5, 20).unwrap()),
        ]),
        Row::new(vec![
            Value::Integer(2),
            Value::String("Bob".to_string()),
            Value::Double(92000.0),
            Value::Boolean(false),
            Value::Date(NaiveDate::from_ymd_opt(1985, 11, 3).unwrap()),
        ]),
        Row::new(vec![
            Value::Integer(3),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
        ]),
    ];

    // 1. Row -> Arrow RecordBatch
    let batch = rows_to_record_batch(&schema, &rows).unwrap();
    assert_eq!(batch.num_rows(), 3);
    assert_eq!(batch.num_columns(), 5);

    // 2. RecordBatch -> Row (Round-trip)
    let restored_0 = record_batch_to_row(&batch, 0).unwrap();
    assert_eq!(restored_0.get(0), Some(&Value::Integer(1)));
    assert_eq!(restored_0.get(1), Some(&Value::String("Alice".to_string())));
    assert_eq!(restored_0.get(2), Some(&Value::Double(75000.5)));
    assert_eq!(restored_0.get(3), Some(&Value::Boolean(true)));
    assert_eq!(restored_0.get(4), Some(&Value::Date(NaiveDate::from_ymd_opt(1990, 5, 20).unwrap())));

    let restored_2 = record_batch_to_row(&batch, 2).unwrap();
    assert_eq!(restored_2.get(0), Some(&Value::Integer(3)));
    assert_eq!(restored_2.get(1), Some(&Value::Null));
    assert_eq!(restored_2.get(2), Some(&Value::Null));
}

#[test]
fn test_vector_chunk_with_selection_mask() {
    let columns = vec![
        ColumnDef::new("val".to_string(), H2DataType::Integer, false, false),
    ];
    let schema = create_arrow_schema(&columns);

    let rows: Vec<Row> = (0..10).map(|i| Row::new(vec![Value::Integer(i)])).collect();
    let batch = rows_to_record_batch(&schema, &rows).unwrap();

    // 奇数行のみを選択するマスク (1, 3, 5, 7, 9)
    let mask_values: Vec<bool> = (0..10).map(|i| i % 2 == 1).collect();
    let mask = Arc::new(BooleanArray::from(mask_values));

    let chunk = VectorChunk::with_selection(batch, mask);
    assert_eq!(chunk.physical_rows(), 10);
    assert_eq!(chunk.num_rows(), 5);

    // 論理行インデックスによるアクセス
    let r0 = chunk.extract_row(0).unwrap();
    assert_eq!(r0.get(0), Some(&Value::Integer(1)));

    let r4 = chunk.extract_row(4).unwrap();
    assert_eq!(r4.get(0), Some(&Value::Integer(9)));

    // マスク適用後の RecordBatch 抽出
    let compacted = chunk.to_record_batch().unwrap();
    assert_eq!(compacted.num_rows(), 5);
}

#[test]
fn test_hybrid_boundary_adapters() {
    let columns = vec![
        ColumnDef::new("id".to_string(), H2DataType::BigInt, false, false),
    ];

    let total_rows = 2500;
    let rows: Vec<Row> = (0..total_rows)
        .map(|i| Row::new(vec![Value::BigInt(i)]))
        .collect();

    // 1. 行ベースオペレータ
    let row_op = Box::new(MemoryRowOperator::new(rows));

    // 2. 行 -> ベクトル化アダプタ (バッチサイズ 1024)
    let vec_adapter = Box::new(RowToVectorAdapter::new(row_op, &columns).with_batch_size(1024));
    assert_eq!(vec_adapter.execution_mode(), ExecutionMode::Vectorized);

    // 3. ベクトル化 -> 行アダプタ
    let mut row_adapter = VectorToRowAdapter::new(vec_adapter);
    assert_eq!(row_adapter.execution_mode(), ExecutionMode::Row);

    // 4. 行ストリームの検証
    let mut count = 0;
    while let Some(row) = row_adapter.next_row().unwrap() {
        assert_eq!(row.get(0), Some(&Value::BigInt(count as i64)));
        count += 1;
    }
    assert_eq!(count, total_rows);
}

#[test]
fn test_vectorized_filter_and_aggregate() {
    let columns = vec![
        ColumnDef::new("category".to_string(), H2DataType::Integer, false, false),
        ColumnDef::new("amount".to_string(), H2DataType::Double, false, false),
    ];
    let schema = create_arrow_schema(&columns);

    let mut rows = Vec::new();
    for i in 0..100 {
        rows.push(Row::new(vec![
            Value::Integer(i % 2),
            Value::Double((i * 10) as f64),
        ]));
    }

    let batch = rows_to_record_batch(&schema, &rows).unwrap();
    let chunk = VectorChunk::new(batch);
    let mem_op = Box::new(MemoryBatchOperator::single(chunk));

    // category == 1 の行のみをフィルタ
    let filtered_op = Box::new(VectorizedFilter::new(mem_op, |chk| {
        let cat_col = chk.columns()[0].as_any().downcast_ref::<Int32Array>().unwrap();
        let mut mask_builder = BooleanBuilder::with_capacity(cat_col.len());
        for i in 0..cat_col.len() {
            mask_builder.append_value(cat_col.value(i) == 1);
        }
        Ok(mask_builder.finish())
    }));

    // 集計: COUNT(*), SUM(amount), AVG(amount), MIN(amount), MAX(amount)
    let agg_ops = vec![
        VectorAggregateOp::CountStar,
        VectorAggregateOp::Sum(1),
        VectorAggregateOp::Avg(1),
        VectorAggregateOp::Min(1),
        VectorAggregateOp::Max(1),
    ];
    let mut agg = VectorizedAggregate::new(filtered_op, agg_ops);
    let result_row = agg.execute_aggregate().unwrap();

    // 奇数インデックス: i = 1, 3, 5, ..., 99 (計 50行)
    // amount = 10, 30, 50, ..., 990
    // sum = 10 * (1 + 3 + ... + 99) = 10 * 2500 = 25000.0
    // avg = 25000.0 / 50 = 500.0
    // min = 10.0, max = 990.0
    assert_eq!(result_row.get(0), Some(&Value::BigInt(50)));
    assert_eq!(result_row.get(1), Some(&Value::Double(25000.0)));
    assert_eq!(result_row.get(2), Some(&Value::Double(500.0)));
    assert_eq!(result_row.get(3), Some(&Value::Double(10.0)));
    assert_eq!(result_row.get(4), Some(&Value::Double(990.0)));
}

#[test]
fn test_slotted_page_direct_transposition() {
    let mut page_buf = [0u8; PAGE_SIZE];
    SlottedPage::init(&mut page_buf, 1, 0);

    let columns = vec![
        ColumnDef::new("id".to_string(), H2DataType::Integer, false, true),
        ColumnDef::new("val".to_string(), H2DataType::Double, false, false),
    ];
    let schema = create_arrow_schema(&columns);

    // ページに 5 件のタプルを挿入
    for i in 0..5 {
        let row = Row::new(vec![Value::Integer(i), Value::Double((i * 100) as f64)]);
        let bytes = row.to_bytes().unwrap();
        assert!(SlottedPage::insert_tuple(&mut page_buf, &bytes).is_some());
    }

    // Slotted Page から直接 VectorChunk を構築
    let chunk = scan_slotted_page_to_batch(&page_buf, &schema, None).unwrap();
    assert_eq!(chunk.num_rows(), 5);

    let r0 = chunk.extract_row(0).unwrap();
    assert_eq!(r0.get(0), Some(&Value::Integer(0)));
    assert_eq!(r0.get(1), Some(&Value::Double(0.0)));

    let r4 = chunk.extract_row(4).unwrap();
    assert_eq!(r4.get(0), Some(&Value::Integer(4)));
    assert_eq!(r4.get(1), Some(&Value::Double(400.0)));
}

#[test]
fn test_sql_execution_mode_and_vectorized_aggregation() {
    let store = Arc::new(MVStore::open_in_memory());
    let engine = SQLEngine::new(store).unwrap();

    // 1. execution_mode の確認と切り替え
    let res = engine.execute("SHOW execution_mode").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows[0].get(0), Some(&Value::String("auto".to_string())));
    }

    engine.execute("SET execution_mode = 'vectorized'").unwrap();
    let res = engine.execute("SHOW execution_mode").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows[0].get(0), Some(&Value::String("vectorized".to_string())));
    }

    // 2. テーブル作成とデータ投入
    engine.execute("CREATE TABLE metrics (id INTEGER, val DOUBLE)").unwrap();
    for i in 1..=100 {
        engine.execute(&format!("INSERT INTO metrics VALUES ({}, {})", i, (i * 2) as f64)).unwrap();
    }

    // 3. EXPLAIN で Vectorized (Apache Arrow) が選択されていることを確認
    let explain_res = engine.execute("EXPLAIN SELECT count(*), sum(val) FROM metrics").unwrap();
    if let ExecutionResult::Query { rows, .. } = explain_res {
        let plan_str = rows[0].get(0).unwrap().to_string();
        assert!(plan_str.contains("Vectorized (Apache Arrow)"));
    }

    // 4. ベクトル化集約の実行
    // count(*) = 100
    // sum(val) = 2 * (1 + ... + 100) = 2 * 5050 = 10100.0
    // avg(val) = 101.0
    // min(val) = 2.0, max(val) = 200.0
    let query_res = engine.execute("SELECT count(*), sum(val), avg(val), min(val), max(val) FROM metrics").unwrap();
    if let ExecutionResult::Query { rows, .. } = query_res {
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get(0), Some(&Value::BigInt(100)));
        assert_eq!(rows[0].get(1), Some(&Value::Double(10100.0)));
        assert_eq!(rows[0].get(2), Some(&Value::Double(101.0)));
        assert_eq!(rows[0].get(3), Some(&Value::Double(2.0)));
        assert_eq!(rows[0].get(4), Some(&Value::Double(200.0)));
    }

    // 5. 行モードに切り替えて等価な結果が得られることを確認 (HTAP 両立)
    engine.execute("SET execution_mode = 'row'").unwrap();
    let row_mode_res = engine.execute("SELECT count(*), sum(val), avg(val), min(val), max(val) FROM metrics").unwrap();
    if let ExecutionResult::Query { rows, .. } = row_mode_res {
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get(0), Some(&Value::BigInt(100)));
        assert_eq!(rows[0].get(1), Some(&Value::Double(10100.0)));
        assert_eq!(rows[0].get(2), Some(&Value::Double(101.0)));
        assert_eq!(rows[0].get(3), Some(&Value::Double(2.0)));
        assert_eq!(rows[0].get(4), Some(&Value::Double(200.0)));
    }
}
