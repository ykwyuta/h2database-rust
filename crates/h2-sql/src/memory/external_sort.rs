use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use uuid::Uuid;

use h2_types::{H2Error, H2Result, Value};
use crate::row::Row;

static TEMP_FILE_SEQ: AtomicU64 = AtomicU64::new(0);

/// ディスク上に退避されたソート済み一時ファイル (Run)
pub struct TempRun {
    path: PathBuf,
    reader: Option<BufReader<File>>,
    num_rows: usize,
}

impl TempRun {
    pub fn create_from_rows(
        rows: &[Row],
    ) -> H2Result<Self> {
        let seq = TEMP_FILE_SEQ.fetch_add(1, AtomicOrdering::Relaxed);
        let filename = format!("h2_sort_spill_{}_{}.tmp", Uuid::new_v4(), seq);
        let path = std::env::temp_dir().join(filename);

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .map_err(|e| H2Error::Storage(format!("Failed to create temp spill file: {}", e)))?;

        let mut writer = BufWriter::new(file);

        for row in rows {
            let bytes = row.to_bytes()?;
            let len_bytes = (bytes.len() as u32).to_be_bytes();
            writer.write_all(&len_bytes)
                .map_err(|e| H2Error::Storage(e.to_string()))?;
            writer.write_all(&bytes)
                .map_err(|e| H2Error::Storage(e.to_string()))?;
        }
        writer.flush().map_err(|e| H2Error::Storage(e.to_string()))?;

        // 読み取り用に再度オープン
        let read_file = File::open(&path)
            .map_err(|e| H2Error::Storage(e.to_string()))?;

        Ok(Self {
            path,
            reader: Some(BufReader::new(read_file)),
            num_rows: rows.len(),
        })
    }

    /// 次の行を読み出し
    pub fn next_row(&mut self) -> H2Result<Option<Row>> {
        if let Some(ref mut reader) = self.reader {
            let mut len_bytes = [0u8; 4];
            match reader.read_exact(&mut len_bytes) {
                Ok(()) => {
                    let len = u32::from_be_bytes(len_bytes) as usize;
                    let mut buf = vec![0u8; len];
                    reader.read_exact(&mut buf)
                        .map_err(|e| H2Error::Storage(e.to_string()))?;
                    let row = Row::from_bytes(&buf)?;
                    Ok(Some(row))
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
                Err(e) => Err(H2Error::Storage(e.to_string())),
            }
        } else {
            Ok(None)
        }
    }

    pub fn num_rows(&self) -> usize {
        self.num_rows
    }
}

impl Drop for TempRun {
    fn drop(&mut self) {
        self.reader = None; // ファイルハンドルを閉じる
        let _ = std::fs::remove_file(&self.path);
    }
}

/// K-way マージ用の優先度付きキューエントリ
struct MergeEntry<'a> {
    row: Row,
    run_index: usize,
    compare_fn: &'a Box<dyn Fn(&Row, &Row) -> Ordering + 'a>,
}

impl<'a> PartialEq for MergeEntry<'a> {
    fn eq(&self, other: &Self) -> bool {
        (self.compare_fn)(&self.row, &other.row) == Ordering::Equal
    }
}

impl<'a> Eq for MergeEntry<'a> {}

impl<'a> PartialOrd for MergeEntry<'a> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<'a> Ord for MergeEntry<'a> {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap は最大ヒープなので、昇順ソートのためには比較順を逆転 (Reverse)
        (self.compare_fn)(&other.row, &self.row)
    }
}

/// 外部マージソート (Phase 3: work_mem 超過時のディスクスピルソート)
pub struct ExternalSorter<'a> {
    work_mem: usize,
    compare_fn: Box<dyn Fn(&Row, &Row) -> Ordering + 'a>,
    buffer: Vec<Row>,
    current_buffer_bytes: usize,
    spilled_runs: Vec<TempRun>,
    total_rows: usize,
}

impl<'a> ExternalSorter<'a> {
    pub fn new<F>(work_mem: usize, compare_fn: F) -> Self
    where
        F: Fn(&Row, &Row) -> Ordering + 'a,
    {
        Self {
            work_mem,
            compare_fn: Box::new(compare_fn),
            buffer: Vec::new(),
            current_buffer_bytes: 0,
            spilled_runs: Vec::new(),
            total_rows: 0,
        }
    }

    /// 行の追加。work_mem を超えたら自動的に一時ファイルにソート済み Run を Spill する
    pub fn add_row(&mut self, row: Row) -> H2Result<()> {
        let approx_row_bytes = row.values.iter().map(|v| match v {
            Value::String(s) => s.len() + 16,
            Value::Bytes(b) => b.len() + 16,
            Value::Json(j) => j.to_string().len() + 16,
            _ => 16,
        }).sum::<usize>() + 24;

        self.buffer.push(row);
        self.current_buffer_bytes += approx_row_bytes;
        self.total_rows += 1;

        if self.current_buffer_bytes >= self.work_mem {
            self.spill_buffer()?;
        }

        Ok(())
    }

    fn spill_buffer(&mut self) -> H2Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        // バッファ内クイックソート
        self.buffer.sort_by(|a, b| (self.compare_fn)(a, b));

        // 一時ファイルへフラッシュ
        let run = TempRun::create_from_rows(&self.buffer)?;
        self.spilled_runs.push(run);

        self.buffer.clear();
        self.current_buffer_bytes = 0;
        Ok(())
    }

    /// ソートを完遂し、結果行を返す
    pub fn finish(mut self) -> H2Result<Vec<Row>> {
        if self.spilled_runs.is_empty() {
            // ディスクスピルが発生しなかった場合: インメモリで直接ソートして終了（最速）
            self.buffer.sort_by(|a, b| (self.compare_fn)(a, b));
            return Ok(self.buffer);
        }

        // 残りのバッファがあれば最後の Run としてスピル
        if !self.buffer.is_empty() {
            self.spill_buffer()?;
        }

        // K-way マージソート (PriorityQueue)
        let _num_runs = self.spilled_runs.len();
        let mut result = Vec::with_capacity(self.total_rows);

        let compare_ref: &'a Box<dyn Fn(&Row, &Row) -> Ordering + 'a> =
            unsafe { std::mem::transmute(&self.compare_fn) };

        let mut heap = BinaryHeap::new();

        // 各 Run から最初の 1 行をロード
        for (run_idx, run) in self.spilled_runs.iter_mut().enumerate() {
            if let Some(first_row) = run.next_row()? {
                heap.push(MergeEntry {
                    row: first_row,
                    run_index: run_idx,
                    compare_fn: compare_ref,
                });
            }
        }

        // 最小要素を取り出して結果に追加し、該当 Run から次行を補充
        while let Some(smallest) = heap.pop() {
            let run_idx = smallest.run_index;
            result.push(smallest.row);

            if let Some(next_row) = self.spilled_runs[run_idx].next_row()? {
                heap.push(MergeEntry {
                    row: next_row,
                    run_index: run_idx,
                    compare_fn: compare_ref,
                });
            }
        }

        Ok(result)
    }

    pub fn spilled_runs_count(&self) -> usize {
        self.spilled_runs.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_in_memory_sort() {
        let mut sorter = ExternalSorter::new(1024 * 1024, |a: &Row, b: &Row| {
            a.values[0].partial_cmp(&b.values[0]).unwrap_or(Ordering::Equal)
        });

        sorter.add_row(Row::new(vec![Value::Integer(5)])).unwrap();
        sorter.add_row(Row::new(vec![Value::Integer(1)])).unwrap();
        sorter.add_row(Row::new(vec![Value::Integer(3)])).unwrap();

        let sorted = sorter.finish().unwrap();
        assert_eq!(sorted.len(), 3);
        assert_eq!(sorted[0].values[0], Value::Integer(1));
        assert_eq!(sorted[1].values[0], Value::Integer(3));
        assert_eq!(sorted[2].values[0], Value::Integer(5));
    }

    #[test]
    fn test_external_merge_sort_spill() {
        // 小さな work_mem (128 バイト) を指定してスピルを強制発動
        let mut sorter = ExternalSorter::new(128, |a: &Row, b: &Row| {
            a.values[0].partial_cmp(&b.values[0]).unwrap_or(Ordering::Equal)
        });

        for val in [50, 20, 80, 10, 40, 70, 30, 60, 90, 5].into_iter() {
            sorter.add_row(Row::new(vec![
                Value::Integer(val),
                Value::String(format!("row_with_padding_{:04}", val)),
            ])).unwrap();
        }

        assert!(sorter.spilled_runs_count() > 0, "Spill should have occurred");

        let sorted = sorter.finish().unwrap();
        assert_eq!(sorted.len(), 10);
        let vals: Vec<i32> = sorted.into_iter().map(|r| match r.values[0] {
            Value::Integer(v) => v,
            _ => panic!(),
        }).collect();

        assert_eq!(vals, vec![5, 10, 20, 30, 40, 50, 60, 70, 80, 90]);
    }
}
