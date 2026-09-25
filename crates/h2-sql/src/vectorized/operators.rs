use std::sync::Arc;
use arrow::array::*;
use arrow::datatypes::*;

use h2_types::{H2Error, H2Result};
use crate::catalog::ColumnDef;
use crate::row::Row;
use super::chunk::VectorChunk;
use super::types::{create_arrow_schema, rows_to_record_batch};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    Row,
    Vectorized,
}

/// ハイブリッド実行オペレータ Trait
pub trait PhysicalOperator: Send + Sync {
    fn execution_mode(&self) -> ExecutionMode;

    /// 行単位でのデータ取得（OLTP・少行時）
    fn next_row(&mut self) -> H2Result<Option<Row>> {
        Err(H2Error::Execution(
            "Row execution is not supported on this operator".into(),
        ))
    }

    /// ベクトル化バッチでのデータ取得（OLAP・大量データ時）
    fn next_batch(&mut self) -> H2Result<Option<VectorChunk>> {
        Err(H2Error::Execution(
            "Vectorized execution is not supported on this operator".into(),
        ))
    }
}

/// 行のリストを順次出力するインメモリ PhysicalOperator
pub struct MemoryRowOperator {
    rows: std::vec::IntoIter<Row>,
}

impl MemoryRowOperator {
    pub fn new(rows: Vec<Row>) -> Self {
        Self {
            rows: rows.into_iter(),
        }
    }
}

impl PhysicalOperator for MemoryRowOperator {
    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Row
    }

    fn next_row(&mut self) -> H2Result<Option<Row>> {
        Ok(self.rows.next())
    }
}

/// 単一または複数の VectorChunk を順次出力するインメモリ PhysicalOperator
pub struct MemoryBatchOperator {
    chunks: std::vec::IntoIter<VectorChunk>,
}

impl MemoryBatchOperator {
    pub fn new(chunks: Vec<VectorChunk>) -> Self {
        Self {
            chunks: chunks.into_iter(),
        }
    }

    pub fn single(chunk: VectorChunk) -> Self {
        Self {
            chunks: vec![chunk].into_iter(),
        }
    }
}

impl PhysicalOperator for MemoryBatchOperator {
    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Vectorized
    }

    fn next_batch(&mut self) -> H2Result<Option<VectorChunk>> {
        Ok(self.chunks.next())
    }
}

/// 行ベースの子オペレータから行を読み出し、VectorChunk (RecordBatch) へ集約して出力するアダプタ
pub struct RowToVectorAdapter {
    child: Box<dyn PhysicalOperator>,
    schema: SchemaRef,
    batch_size: usize,
}

impl RowToVectorAdapter {
    pub fn new(child: Box<dyn PhysicalOperator>, columns: &[ColumnDef]) -> Self {
        let schema = create_arrow_schema(columns);
        Self {
            child,
            schema,
            batch_size: VectorChunk::DEFAULT_CHUNK_SIZE,
        }
    }

    pub fn with_batch_size(mut self, size: usize) -> Self {
        self.batch_size = size;
        self
    }
}

impl PhysicalOperator for RowToVectorAdapter {
    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Vectorized
    }

    fn next_batch(&mut self) -> H2Result<Option<VectorChunk>> {
        let mut buffer = Vec::with_capacity(self.batch_size);

        while buffer.len() < self.batch_size {
            match self.child.next_row()? {
                Some(row) => buffer.push(row),
                None => break,
            }
        }

        if buffer.is_empty() {
            return Ok(None);
        }

        let batch = rows_to_record_batch(&self.schema, &buffer)?;
        Ok(Some(VectorChunk::new(batch)))
    }
}

/// ベクトル化子オペレータから VectorChunk を受け取り、行単位の Row として順次アンパックするアダプタ
pub struct VectorToRowAdapter {
    child: Box<dyn PhysicalOperator>,
    current_chunk: Option<VectorChunk>,
    current_logical_idx: usize,
}

impl VectorToRowAdapter {
    pub fn new(child: Box<dyn PhysicalOperator>) -> Self {
        Self {
            child,
            current_chunk: None,
            current_logical_idx: 0,
        }
    }
}

impl PhysicalOperator for VectorToRowAdapter {
    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Row
    }

    fn next_row(&mut self) -> H2Result<Option<Row>> {
        loop {
            if let Some(ref chunk) = self.current_chunk {
                if self.current_logical_idx < chunk.num_rows() {
                    let row = chunk.extract_row(self.current_logical_idx)?;
                    self.current_logical_idx += 1;
                    return Ok(Some(row));
                }
            }

            // 現在のチャンクを消費し切った場合、次のバッチを取得
            let next_opt: Option<VectorChunk> = self.child.next_batch()?;
            match next_opt {
                Some(chunk) => {
                    if chunk.is_empty() {
                        continue;
                    }
                    self.current_chunk = Some(chunk);
                    self.current_logical_idx = 0;
                }
                None => {
                    self.current_chunk = None;
                    return Ok(None);
                }
            }
        }
    }
}

/// ベクトル化フィルタオペレータ（述語評価と Selection Vector の生成）
pub struct VectorizedFilter {
    child: Box<dyn PhysicalOperator>,
    filter_fn: Box<dyn Fn(&VectorChunk) -> H2Result<BooleanArray> + Send + Sync>,
}

impl VectorizedFilter {
    pub fn new<F>(child: Box<dyn PhysicalOperator>, filter_fn: F) -> Self
    where
        F: Fn(&VectorChunk) -> H2Result<BooleanArray> + Send + Sync + 'static,
    {
        Self {
            child,
            filter_fn: Box::new(filter_fn),
        }
    }
}

impl PhysicalOperator for VectorizedFilter {
    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Vectorized
    }

    fn next_batch(&mut self) -> H2Result<Option<VectorChunk>> {
        while let Some(chunk) = self.child.next_batch()? {
            let mask = (self.filter_fn)(&chunk)?;

            // 既存の選択マスクと AND 結合
            let combined_mask = match chunk.selection() {
                None => Arc::new(mask),
                Some(existing) => {
                    let and_res = arrow::compute::and(existing.as_ref(), &mask)
                        .map_err(|e| H2Error::Execution(format!("Arrow compute and error: {}", e)))?;
                    Arc::new(and_res)
                }
            };

            let filtered_chunk = VectorChunk::with_selection(chunk.raw_batch().clone(), combined_mask);
            if !filtered_chunk.is_empty() {
                return Ok(Some(filtered_chunk));
            }
            // 空の場合は次のチャンクへ
        }
        Ok(None)
    }
}

/// ベクトル化集約オペレータ（SUM, COUNT, MIN, MAX, AVG）
#[derive(Debug, Clone)]
pub enum VectorAggregateOp {
    CountStar,
    Count(usize),
    Sum(usize),
    Min(usize),
    Max(usize),
    Avg(usize),
}

pub struct VectorizedAggregate {
    child: Box<dyn PhysicalOperator>,
    ops: Vec<VectorAggregateOp>,
    executed: bool,
}

impl VectorizedAggregate {
    pub fn new(child: Box<dyn PhysicalOperator>, ops: Vec<VectorAggregateOp>) -> Self {
        Self {
            child,
            ops,
            executed: false,
        }
    }

    /// 全バッチを走査して集計を実行し、結果行を返す
    pub fn execute_aggregate(&mut self) -> H2Result<Row> {
        let mut count_star: i64 = 0;
        let mut sums: Vec<f64> = vec![0.0; self.ops.len()];
        let mut counts: Vec<i64> = vec![0; self.ops.len()];
        let mut mins: Vec<Option<f64>> = vec![None; self.ops.len()];
        let mut maxs: Vec<Option<f64>> = vec![None; self.ops.len()];

        while let Some(chunk) = self.child.next_batch()? {
            let compacted = chunk.to_record_batch()?;
            let num_rows = compacted.num_rows();
            count_star += num_rows as i64;

            for (op_idx, op) in self.ops.iter().enumerate() {
                match op {
                    VectorAggregateOp::CountStar => {}
                    VectorAggregateOp::Count(col_idx) => {
                        let col = compacted.column(*col_idx);
                        let valid_count = col.len() - col.null_count();
                        counts[op_idx] += valid_count as i64;
                    }
                    VectorAggregateOp::Sum(col_idx) | VectorAggregateOp::Avg(col_idx) => {
                        let col = compacted.column(*col_idx);
                        if let Some(arr) = col.as_any().downcast_ref::<Int32Array>() {
                            if let Some(s) = arrow::compute::sum(arr) {
                                sums[op_idx] += s as f64;
                                counts[op_idx] += (arr.len() - arr.null_count()) as i64;
                            }
                        } else if let Some(arr) = col.as_any().downcast_ref::<Int64Array>() {
                            if let Some(s) = arrow::compute::sum(arr) {
                                sums[op_idx] += s as f64;
                                counts[op_idx] += (arr.len() - arr.null_count()) as i64;
                            }
                        } else if let Some(arr) = col.as_any().downcast_ref::<Float64Array>() {
                            if let Some(s) = arrow::compute::sum(arr) {
                                sums[op_idx] += s;
                                counts[op_idx] += (arr.len() - arr.null_count()) as i64;
                            }
                        }
                    }
                    VectorAggregateOp::Min(col_idx) => {
                        let col = compacted.column(*col_idx);
                        if let Some(arr) = col.as_any().downcast_ref::<Int32Array>() {
                            if let Some(m) = arrow::compute::min(arr) {
                                let v = m as f64;
                                mins[op_idx] = Some(mins[op_idx].map_or(v, |curr| curr.min(v)));
                            }
                        } else if let Some(arr) = col.as_any().downcast_ref::<Int64Array>() {
                            if let Some(m) = arrow::compute::min(arr) {
                                let v = m as f64;
                                mins[op_idx] = Some(mins[op_idx].map_or(v, |curr| curr.min(v)));
                            }
                        } else if let Some(arr) = col.as_any().downcast_ref::<Float64Array>() {
                            if let Some(m) = arrow::compute::min(arr) {
                                mins[op_idx] = Some(mins[op_idx].map_or(m, |curr| curr.min(m)));
                            }
                        }
                    }
                    VectorAggregateOp::Max(col_idx) => {
                        let col = compacted.column(*col_idx);
                        if let Some(arr) = col.as_any().downcast_ref::<Int32Array>() {
                            if let Some(m) = arrow::compute::max(arr) {
                                let v = m as f64;
                                maxs[op_idx] = Some(maxs[op_idx].map_or(v, |curr| curr.max(v)));
                            }
                        } else if let Some(arr) = col.as_any().downcast_ref::<Int64Array>() {
                            if let Some(m) = arrow::compute::max(arr) {
                                let v = m as f64;
                                maxs[op_idx] = Some(maxs[op_idx].map_or(v, |curr| curr.max(v)));
                            }
                        } else if let Some(arr) = col.as_any().downcast_ref::<Float64Array>() {
                            if let Some(m) = arrow::compute::max(arr) {
                                maxs[op_idx] = Some(maxs[op_idx].map_or(m, |curr| curr.max(m)));
                            }
                        }
                    }
                }
            }
        }

        let mut res_values = Vec::with_capacity(self.ops.len());
        for (op_idx, op) in self.ops.iter().enumerate() {
            let val = match op {
                VectorAggregateOp::CountStar => h2_types::Value::BigInt(count_star),
                VectorAggregateOp::Count(_) => h2_types::Value::BigInt(counts[op_idx]),
                VectorAggregateOp::Sum(_) => {
                    if counts[op_idx] == 0 {
                        h2_types::Value::Null
                    } else {
                        h2_types::Value::Double(sums[op_idx])
                    }
                }
                VectorAggregateOp::Avg(_) => {
                    if counts[op_idx] == 0 {
                        h2_types::Value::Null
                    } else {
                        h2_types::Value::Double(sums[op_idx] / counts[op_idx] as f64)
                    }
                }
                VectorAggregateOp::Min(_) => {
                    match mins[op_idx] {
                        Some(v) => h2_types::Value::Double(v),
                        None => h2_types::Value::Null,
                    }
                }
                VectorAggregateOp::Max(_) => {
                    match maxs[op_idx] {
                        Some(v) => h2_types::Value::Double(v),
                        None => h2_types::Value::Null,
                    }
                }
            };
            res_values.push(val);
        }

        Ok(Row::new(res_values))
    }
}

impl PhysicalOperator for VectorizedAggregate {
    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Row
    }

    fn next_row(&mut self) -> H2Result<Option<Row>> {
        if self.executed {
            return Ok(None);
        }
        self.executed = true;
        let row = self.execute_aggregate()?;
        Ok(Some(row))
    }
}
