use std::sync::Arc;
use arrow::array::{Array, ArrayRef, BooleanArray};
use arrow::datatypes::SchemaRef;
use arrow::record_batch::RecordBatch;

use h2_types::{H2Error, H2Result};
use crate::row::Row;
use super::types::record_batch_to_row;

/// ベクトル化実行エンジン内を流れるデータの標準単位（通常 1024 行）
#[derive(Clone, Debug)]
pub struct VectorChunk {
    /// Arrow の標準 RecordBatch（各列の連続メモリバッファを内包）
    batch: RecordBatch,
    /// フィルタ処理などで除外された行を物理削除せずスキップするための選択マスク
    /// （Selection Vector / Validity Mask）
    selection: Option<Arc<BooleanArray>>,
}

impl VectorChunk {
    pub const DEFAULT_CHUNK_SIZE: usize = 1024;

    /// 選択マスクなしの VectorChunk を生成
    pub fn new(batch: RecordBatch) -> Self {
        Self {
            batch,
            selection: None,
        }
    }

    /// 選択マスク付きの VectorChunk を生成
    pub fn with_selection(batch: RecordBatch, selection: Arc<BooleanArray>) -> Self {
        Self {
            batch,
            selection: Some(selection),
        }
    }

    /// 物理的な総行数を取得
    pub fn physical_rows(&self) -> usize {
        self.batch.num_rows()
    }

    /// 選択マスクを考慮した論理行数を取得
    pub fn num_rows(&self) -> usize {
        match &self.selection {
            None => self.batch.num_rows(),
            Some(mask) => {
                let mut count = 0;
                for i in 0..mask.len() {
                    if mask.is_valid(i) && mask.value(i) {
                        count += 1;
                    }
                }
                count
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.num_rows() == 0
    }

    pub fn columns(&self) -> &[ArrayRef] {
        self.batch.columns()
    }

    pub fn schema(&self) -> SchemaRef {
        self.batch.schema()
    }

    pub fn raw_batch(&self) -> &RecordBatch {
        &self.batch
    }

    pub fn selection(&self) -> Option<&Arc<BooleanArray>> {
        self.selection.as_ref()
    }

    /// 選択マスクを適用し、圧縮された Arrow RecordBatch を生成
    pub fn to_record_batch(&self) -> H2Result<RecordBatch> {
        match &self.selection {
            None => Ok(self.batch.clone()),
            Some(mask) => {
                arrow::compute::filter_record_batch(&self.batch, mask.as_ref())
                    .map_err(|e| H2Error::Execution(format!("Failed to apply selection mask: {}", e)))
            }
        }
    }

    /// 論理行インデックス（0..num_rows()）から Row を抽出
    pub fn extract_row(&self, logical_idx: usize) -> H2Result<Row> {
        match &self.selection {
            None => record_batch_to_row(&self.batch, logical_idx),
            Some(mask) => {
                let mut seen_logical = 0;
                let mut found_physical = None;

                for i in 0..mask.len() {
                    if mask.is_valid(i) && mask.value(i) {
                        if seen_logical == logical_idx {
                            found_physical = Some(i);
                            break;
                        }
                        seen_logical += 1;
                    }
                }

                if let Some(phys_idx) = found_physical {
                    record_batch_to_row(&self.batch, phys_idx)
                } else {
                    Err(H2Error::Execution(format!(
                        "Logical row index out of bounds: {} >= valid count {}",
                        logical_idx, seen_logical
                    )))
                }
            }
        }
    }
}
