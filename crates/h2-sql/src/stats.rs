use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::SystemTime;

use sqlparser::ast::{BinaryOperator, Expr};

use h2_mvstore::Transaction;
use h2_types::{H2Result, Value};

use crate::catalog::{ColumnStats, TableDef, TableStats};
use crate::expression::evaluate_literal_or_unary;
use crate::row::Row;

/// サンプリング指定オプション
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SampleSpec {
    /// 3000行を超える場合に自動サンプリング（デフォルト標本サイズ: 3000）
    Default,
    /// 明示的な標本行数を指定 (e.g. WITH SAMPLE 1000 ROWS)
    Rows(usize),
    /// 明示的な標本パーセンテージを指定 (e.g. WITH SAMPLE 10 PERCENT)
    Percent(f64),
    /// 100% 全件スキャン
    Full,
}

/// Value を HashMap で高速にカウントするためのラッパー
#[derive(Clone, Debug)]
pub struct HashableValue(pub Value);

impl PartialEq for HashableValue {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for HashableValue {}

impl Hash for HashableValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match &self.0 {
            Value::Null => 0u8.hash(state),
            Value::Boolean(b) => {
                1u8.hash(state);
                b.hash(state);
            }
            Value::TinyInt(i) => {
                2u8.hash(state);
                i.hash(state);
            }
            Value::SmallInt(i) => {
                3u8.hash(state);
                i.hash(state);
            }
            Value::Integer(i) => {
                4u8.hash(state);
                i.hash(state);
            }
            Value::BigInt(i) => {
                5u8.hash(state);
                i.hash(state);
            }
            Value::Float(f) => {
                6u8.hash(state);
                f.to_bits().hash(state);
            }
            Value::Double(d) => {
                7u8.hash(state);
                d.to_bits().hash(state);
            }
            Value::Decimal(d) => {
                8u8.hash(state);
                d.hash(state);
            }
            Value::String(s) => {
                9u8.hash(state);
                s.hash(state);
            }
            Value::Bytes(b) => {
                10u8.hash(state);
                b.hash(state);
            }
            Value::Date(d) => {
                11u8.hash(state);
                d.hash(state);
            }
            Value::Time(t) => {
                12u8.hash(state);
                t.hash(state);
            }
            Value::Timestamp(ts) => {
                13u8.hash(state);
                ts.hash(state);
            }
            Value::Uuid(u) => {
                14u8.hash(state);
                u.hash(state);
            }
            Value::Json(j) => {
                15u8.hash(state);
                j.to_string().hash(state);
            }
            Value::Array(a) => {
                16u8.hash(state);
                for v in a {
                    HashableValue(v.clone()).hash(state);
                }
            }
            Value::Interval(iv) => {
                17u8.hash(state);
                iv.hash(state);
            }
        }
    }
}

/// テーブルの統計情報を収集（ANALYZE デフォルト）
pub fn analyze_table(
    tx: &Transaction,
    table_def: &TableDef,
) -> H2Result<(TableStats, HashMap<String, ColumnStats>)> {
    analyze_table_with_sample(tx, table_def, SampleSpec::Default)
}

/// サンプリング指定付きでテーブル統計情報を収集（ANALYZE WITH SAMPLE ...）
pub fn analyze_table_with_sample(
    tx: &Transaction,
    table_def: &TableDef,
    spec: SampleSpec,
) -> H2Result<(TableStats, HashMap<String, ColumnStats>)> {
    let map_name = table_def.map_name();
    let entries = tx.scan_visible(&map_name)?;
    let row_count = entries.len() as u64;

    if row_count == 0 {
        let mut col_stats_map = HashMap::new();
        for col in &table_def.columns {
            col_stats_map.insert(
                col.name.to_lowercase(),
                ColumnStats {
                    ndv: 0,
                    null_frac: 0.0,
                    avg_width: 8.0,
                    most_common_vals: Vec::new(),
                    most_common_freqs: Vec::new(),
                },
            );
        }
        let now_millis = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let table_stats = TableStats {
            row_count: 0,
            dead_row_count: 0,
            last_analyzed: Some(now_millis),
            total_pages: 1,
            sample_ratio: 1.0,
        };
        return Ok((table_stats, col_stats_map));
    }

    // サンプル行数の決定
    let sample_size = match spec {
        SampleSpec::Full => entries.len(),
        SampleSpec::Rows(r) => r.clamp(1, entries.len()),
        SampleSpec::Percent(p) => {
            let p_clamped = p.clamp(0.01, 100.0);
            ((row_count as f64 * p_clamped / 100.0).round() as usize).clamp(1, entries.len())
        }
        SampleSpec::Default => {
            if row_count > 3000 {
                3000
            } else {
                entries.len()
            }
        }
    };

    let sample_ratio = sample_size as f64 / row_count as f64;

    let mut col_null_counts: Vec<u64> = vec![0; table_def.columns.len()];
    let mut col_val_counts: Vec<HashMap<HashableValue, u64>> =
        vec![HashMap::new(); table_def.columns.len()];
    let mut col_total_bytes: Vec<u64> = vec![0; table_def.columns.len()];

    // 標本サンプリングの抽出（全件走査 or 等間隔システマティックサンプリング）
    if sample_size >= entries.len() {
        for (_k, val_bytes) in &entries {
            let mut row = Row::from_bytes(val_bytes)?;
            table_def.align_row(&mut row);
            process_sampled_row(
                &row,
                table_def,
                &mut col_null_counts,
                &mut col_val_counts,
                &mut col_total_bytes,
            );
        }
    } else {
        let stride = entries.len() as f64 / sample_size as f64;
        for i in 0..sample_size {
            let idx = ((i as f64 * stride).floor() as usize).min(entries.len() - 1);
            let (_k, val_bytes) = &entries[idx];
            let mut row = Row::from_bytes(val_bytes)?;
            table_def.align_row(&mut row);
            process_sampled_row(
                &row,
                table_def,
                &mut col_null_counts,
                &mut col_val_counts,
                &mut col_total_bytes,
            );
        }
    }

    let mut col_stats_map = HashMap::new();
    let mut total_row_avg_width = 0.0;

    for (idx, col) in table_def.columns.iter().enumerate() {
        let null_count = col_null_counts[idx];
        let null_frac = null_count as f64 / sample_size as f64;

        let non_null_count = sample_size as u64 - null_count;
        let avg_width = if non_null_count > 0 {
            col_total_bytes[idx] as f64 / non_null_count as f64
        } else {
            8.0
        };
        total_row_avg_width += avg_width;

        let val_map = &col_val_counts[idx];
        let d = val_map.len() as u64;

        // NDV推計 (Haas-Stokes推計法)
        let ndv = if sample_ratio >= 0.999 || row_count <= sample_size as u64 {
            d
        } else {
            // サンプル内で出現頻度が1回のみの単一出現値（Singletons: f1）をカウント
            let f1 = val_map.values().filter(|&&c| c == 1).count() as f64;
            let n = sample_size as f64;
            let n_total = row_count as f64;
            let estimated_d = d as f64 + f1 * ((n_total - n) / n);
            (estimated_d.round() as u64).clamp(d, row_count)
        };

        // 頻度順にソート（Top 10 MCV）
        let mut val_list: Vec<(Value, u64)> = val_map
            .iter()
            .map(|(hv, count)| (hv.0.clone(), *count))
            .collect();
        val_list.sort_by(|a, b| b.1.cmp(&a.1));

        let mut mcv_vals = Vec::new();
        let mut mcv_freqs = Vec::new();

        for (v, c) in val_list.iter().take(10) {
            mcv_vals.push(v.clone());
            let freq = *c as f64 / sample_size as f64;
            mcv_freqs.push(freq);
        }

        let cs = ColumnStats {
            ndv,
            null_frac,
            avg_width,
            most_common_vals: mcv_vals,
            most_common_freqs: mcv_freqs,
        };

        col_stats_map.insert(col.name.to_lowercase(), cs);
    }

    // 8KB ページ換算での推定ページ数
    let approx_page_size = 8192.0;
    let approx_bytes = row_count as f64 * (total_row_avg_width + 16.0);
    let total_pages = ((approx_bytes / approx_page_size).ceil() as u64).max(1);

    let now_millis = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let table_stats = TableStats {
        row_count,
        dead_row_count: 0,
        last_analyzed: Some(now_millis),
        total_pages,
        sample_ratio,
    };

    Ok((table_stats, col_stats_map))
}

fn process_sampled_row(
    row: &Row,
    table_def: &TableDef,
    col_null_counts: &mut [u64],
    col_val_counts: &mut [HashMap<HashableValue, u64>],
    col_total_bytes: &mut [u64],
) {
    for (idx, val) in row.values.iter().enumerate() {
        if idx >= table_def.columns.len() {
            break;
        }
        if val.is_null() {
            col_null_counts[idx] += 1;
        } else {
            *col_val_counts[idx]
                .entry(HashableValue(val.clone()))
                .or_insert(0) += 1;

            let approx_len = match val {
                Value::String(s) => s.len(),
                Value::Bytes(b) => b.len(),
                Value::Json(j) => j.to_string().len(),
                _ => 8,
            };
            col_total_bytes[idx] += approx_len as u64;
        }
    }
}

/// 式の選択率（0.0 .. 1.0）を推定
pub fn estimate_selectivity(table_def: &TableDef, selection: &Expr) -> f64 {
    match selection {
        Expr::BinaryOp { left, op, right } => {
            match op {
                BinaryOperator::And => {
                    let s1 = estimate_selectivity(table_def, left);
                    let s2 = estimate_selectivity(table_def, right);
                    s1 * s2
                }
                BinaryOperator::Or => {
                    let s1 = estimate_selectivity(table_def, left);
                    let s2 = estimate_selectivity(table_def, right);
                    (s1 + s2 - s1 * s2).clamp(0.0, 1.0)
                }
                BinaryOperator::Eq => {
                    if let Expr::Identifier(ident) = left.as_ref() {
                        let col_name = ident.value.to_lowercase();
                        if let Some(col) = table_def.columns.iter().find(|c| c.name.to_lowercase() == col_name) {
                            if let Some(ref cs) = col.stats {
                                if let Ok(val) = evaluate_literal_or_unary(right) {
                                    // MCV に含まれているか？
                                    if let Some(pos) = cs.most_common_vals.iter().position(|v| v == &val) {
                                        return cs.most_common_freqs.get(pos).cloned().unwrap_or(0.1);
                                    }
                                }
                                if cs.ndv > 0 {
                                    let mcv_sum: f64 = cs.most_common_freqs.iter().sum();
                                    let remaining_prob = (1.0 - cs.null_frac - mcv_sum).max(0.0);
                                    let remaining_ndv = (cs.ndv.saturating_sub(cs.most_common_vals.len() as u64)).max(1);
                                    return (remaining_prob / remaining_ndv as f64).clamp(0.0001, 1.0);
                                }
                            }
                        }
                    }
                    0.1 // デフォルト選択率
                }
                BinaryOperator::NotEq => {
                    let eq_sel = estimate_selectivity(table_def, &Expr::BinaryOp {
                        left: left.clone(),
                        op: BinaryOperator::Eq,
                        right: right.clone(),
                    });
                    (1.0 - eq_sel).clamp(0.0, 1.0)
                }
                BinaryOperator::Gt | BinaryOperator::GtEq | BinaryOperator::Lt | BinaryOperator::LtEq => {
                    0.33 // 不等号のデフォルト選択率 1/3
                }
                _ => 0.2,
            }
        }
        Expr::Between { .. } => 0.25,
        Expr::IsNull(expr) => {
            if let Expr::Identifier(ident) = expr.as_ref() {
                let col_name = ident.value.to_lowercase();
                if let Some(col) = table_def.columns.iter().find(|c| c.name.to_lowercase() == col_name) {
                    if let Some(ref cs) = col.stats {
                        return cs.null_frac;
                    }
                }
            }
            0.05
        }
        Expr::IsNotNull(expr) => {
            let is_null_sel = estimate_selectivity(table_def, &Expr::IsNull(expr.clone()));
            1.0 - is_null_sel
        }
        _ => 0.5,
    }
}

/// スキャンコストの見積もり
pub fn estimate_scan_cost(
    table_def: &TableDef,
    selection: Option<&Expr>,
    is_index_scan: bool,
) -> (f64, u64) {
    let total_rows = if let Some(ref s) = table_def.stats {
        s.row_count
    } else {
        table_def.approx_row_count.max(1) as u64
    };

    let total_pages = if let Some(ref s) = table_def.stats {
        s.total_pages
    } else {
        ((total_rows as f64 * 100.0 / 8192.0).ceil() as u64).max(1)
    };

    let selectivity = selection.map(|e| estimate_selectivity(table_def, e)).unwrap_or(1.0);
    let estimated_rows = ((total_rows as f64 * selectivity).round() as u64).max(1);

    let cost = if is_index_scan {
        // インデックスページ I/O (1.0) + ランダムページアクセス (4.0 * 行数) + CPU 評価
        1.0 + (estimated_rows as f64 * 4.0) + (estimated_rows as f64 * 0.01)
    } else {
        // シーケンシャルページ I/O (1.0 * pages) + タプルCPU評価 (0.01 * rows) + 抽出CPU
        (total_pages as f64 * 1.0) + (total_rows as f64 * 0.01) + (estimated_rows as f64 * 0.0025)
    };

    (cost, estimated_rows)
}

/// 複数テーブルの結合順序を貪欲法（Greedy Algorithm）で選択
pub fn choose_driving_table(
    tables: &[(&TableDef, Option<String>)],
) -> usize {
    if tables.is_empty() {
        return 0;
    }
    // 推定行数が最も小さいテーブルを先頭（Driving Table）にする
    let mut best_idx = 0;
    let mut min_rows = u64::MAX;

    for (idx, (t_def, _)) in tables.iter().enumerate() {
        let rows = t_def.stats.as_ref().map(|s| s.row_count).unwrap_or(t_def.approx_row_count.max(1) as u64);
        if rows < min_rows {
            min_rows = rows;
            best_idx = idx;
        }
    }

    best_idx
}
