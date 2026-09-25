use arrow::datatypes::SchemaRef;
use h2_mvstore::buffer_pool::slotted_page::{SlottedPage, PAGE_SIZE};

use h2_types::H2Result;
use crate::row::Row;
use super::chunk::VectorChunk;
use super::types::rows_to_record_batch;

/// Slotted Page からダイレクトにタプルを読み出し、中間 Row の永続化を介さず Arrow RecordBatch に展開する
pub fn scan_slotted_page_to_batch(
    page_buf: &[u8; PAGE_SIZE],
    schema: &SchemaRef,
    projection: Option<&[usize]>,
) -> H2Result<VectorChunk> {
    let tuple_count = SlottedPage::get_tuple_count(page_buf);
    let mut rows = Vec::with_capacity(tuple_count as usize);

    for slot_id in 0..tuple_count {
        if let Some(bytes) = SlottedPage::get_tuple(page_buf, slot_id) {
            let full_row = Row::from_bytes(bytes)?;

            // プロジェクション適用（必要な列のみ抽出）
            let projected_row = match projection {
                Some(proj_indices) => {
                    let mut proj_values = Vec::with_capacity(proj_indices.len());
                    for &idx in proj_indices {
                        proj_values.push(full_row.get(idx).cloned().unwrap_or(h2_types::Value::Null));
                    }
                    Row::new(proj_values)
                }
                None => full_row,
            };
            rows.push(projected_row);
        }
    }

    let batch = rows_to_record_batch(schema, &rows)?;
    Ok(VectorChunk::new(batch))
}
