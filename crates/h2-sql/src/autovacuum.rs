use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::SystemTime;
use parking_lot::RwLock;

use h2_mvstore::TransactionStore;
use h2_types::H2Result;

use crate::catalog::Catalog;
use crate::stats::{analyze_table_with_sample, SampleSpec};

/// AutoVacuum / AutoAnalyze の閾値設定
#[derive(Debug, Clone)]
pub struct AutoVacuumConfig {
    pub vacuum_threshold: u64,     // デフォルト 50
    pub vacuum_scale_factor: f64,  // デフォルト 0.2 (20%)
    pub analyze_threshold: u64,    // デフォルト 50
    pub analyze_scale_factor: f64, // デフォルト 0.1 (10%)
    pub iceberg_compaction_file_threshold: usize, // デフォルト 10
    pub enabled: bool,             // デフォルト true
}

impl Default for AutoVacuumConfig {
    fn default() -> Self {
        Self {
            vacuum_threshold: 50,
            vacuum_scale_factor: 0.2,
            analyze_threshold: 50,
            analyze_scale_factor: 0.1,
            iceberg_compaction_file_threshold: 10,
            enabled: true,
        }
    }
}

/// テーブルごとの更新・デッドタプル・実行履歴
pub struct TableActivity {
    pub mod_count: AtomicU64,
    pub dead_tuple_count: AtomicU64,
    pub last_vacuum_time: AtomicU64,
    pub last_analyze_time: AtomicU64,
}

impl TableActivity {
    pub fn new() -> Self {
        Self {
            mod_count: AtomicU64::new(0),
            dead_tuple_count: AtomicU64::new(0),
            last_vacuum_time: AtomicU64::new(0),
            last_analyze_time: AtomicU64::new(0),
        }
    }
}

/// AutoVacuum & AutoAnalyze を統括するコーディネーター
pub struct AutoVacuumCoordinator {
    config: RwLock<AutoVacuumConfig>,
    table_activities: RwLock<HashMap<String, Arc<TableActivity>>>,
    pub is_running: AtomicBool,
}

impl AutoVacuumCoordinator {
    pub fn new(config: AutoVacuumConfig) -> Self {
        Self {
            config: RwLock::new(config),
            table_activities: RwLock::new(HashMap::new()),
            is_running: AtomicBool::new(false),
        }
    }

    pub fn config(&self) -> AutoVacuumConfig {
        self.config.read().clone()
    }

    pub fn set_config(&self, config: AutoVacuumConfig) {
        *self.config.write() = config;
    }

    pub fn get_or_create_activity(&self, table_name: &str) -> Arc<TableActivity> {
        let key = table_name.to_lowercase();
        let mut map = self.table_activities.write();
        map.entry(key)
            .or_insert_with(|| Arc::new(TableActivity::new()))
            .clone()
    }

    /// INSERT 実行時の更新行数を記録
    pub fn record_insert(&self, table_name: &str, count: u64) {
        let act = self.get_or_create_activity(table_name);
        act.mod_count.fetch_add(count, Ordering::SeqCst);
    }

    /// UPDATE 実行時の更新行数を記録（更新は新世代タプル作成と旧世代のデッド化を伴う）
    pub fn record_update(&self, table_name: &str, count: u64) {
        let act = self.get_or_create_activity(table_name);
        act.mod_count.fetch_add(count, Ordering::SeqCst);
        act.dead_tuple_count.fetch_add(count, Ordering::SeqCst);
    }

    /// DELETE 実行時の削除行数を記録
    pub fn record_delete(&self, table_name: &str, count: u64) {
        let act = self.get_or_create_activity(table_name);
        act.mod_count.fetch_add(count, Ordering::SeqCst);
        act.dead_tuple_count.fetch_add(count, Ordering::SeqCst);
    }

    /// VACUUM が必要か判定: dead_tuples >= threshold + scale_factor * row_count
    pub fn needs_vacuum(&self, table_name: &str, current_rows: u64) -> bool {
        let cfg = self.config.read();
        if !cfg.enabled {
            return false;
        }
        let act = self.get_or_create_activity(table_name);
        let dead = act.dead_tuple_count.load(Ordering::SeqCst);
        let threshold = cfg.vacuum_threshold as f64 + (cfg.vacuum_scale_factor * current_rows as f64);
        dead as f64 >= threshold
    }

    /// ANALYZE が必要か判定: mod_count >= threshold + scale_factor * row_count
    pub fn needs_analyze(&self, table_name: &str, current_rows: u64) -> bool {
        let cfg = self.config.read();
        if !cfg.enabled {
            return false;
        }
        let act = self.get_or_create_activity(table_name);
        let modified = act.mod_count.load(Ordering::SeqCst);
        let threshold = cfg.analyze_threshold as f64 + (cfg.analyze_scale_factor * current_rows as f64);
        modified as f64 >= threshold
    }

    /// Iceberg テーブルのコンパクション要否判定
    pub fn needs_iceberg_compaction(&self, table_def: &crate::catalog::TableDef) -> bool {
        let cfg = self.config.read();
        if !cfg.enabled || !table_def.is_iceberg {
            return false;
        }
        let act = self.get_or_create_activity(&table_def.name);
        let dead = act.dead_tuple_count.load(Ordering::SeqCst);
        let current_rows = table_def.approx_row_count.max(1) as u64;
        let dead_threshold = cfg.vacuum_threshold as f64 + (cfg.vacuum_scale_factor * current_rows as f64);

        let location = match table_def.iceberg_location.as_deref() {
            Some(loc) => loc,
            None => return false,
        };
        if let Ok((_ver, meta)) = crate::iceberg::get_latest_metadata(location) {
            if let Some(snap_id) = meta.current_snapshot_id {
                if let Some(snap) = meta.snapshots.iter().find(|s| s.snapshot_id == snap_id) {
                    if let Ok(m_list) = crate::iceberg::read_manifest_list(location, &snap.manifest_list) {
                        let mut data_count = 0;
                        let mut delete_count = 0;
                        for me in m_list {
                            if let Ok(entries) = crate::iceberg::read_manifest_file(location, &me.manifest_path) {
                                for e in entries {
                                    if e.status != 2 {
                                        if e.data_file.content == 1 || e.data_file.content == 2 {
                                            delete_count += 1;
                                        } else {
                                            data_count += 1;
                                        }
                                    }
                                }
                            }
                        }
                        if data_count >= cfg.iceberg_compaction_file_threshold {
                            return true;
                        }
                        if delete_count > 0 && dead as f64 >= dead_threshold {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    /// 指定テーブルに対して必要に応じ AutoVacuum / AutoAnalyze / Iceberg Compaction を実行
    pub fn run_maintenance_for_table(
        &self,
        tx_store: &Arc<TransactionStore>,
        catalog: &Arc<Catalog>,
        table_name: &str,
    ) -> H2Result<(bool, bool)> {
        let table_def = match catalog.get_table(table_name) {
            Some(t) => t,
            None => return Ok((false, false)),
        };

        if table_def.is_iceberg {
            if self.needs_iceberg_compaction(&table_def) {
                let res = crate::iceberg::compact_iceberg_table(&table_def)?;
                let act = self.get_or_create_activity(table_name);
                let now_millis = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                act.dead_tuple_count.store(0, Ordering::SeqCst);
                act.last_vacuum_time.store(now_millis, Ordering::SeqCst);
                return Ok((res.compacted, false));
            }
            return Ok((false, false));
        }

        let current_rows = table_def
            .stats
            .as_ref()
            .map(|s| s.row_count)
            .unwrap_or(table_def.approx_row_count.max(1) as u64);

        let do_vacuum = self.needs_vacuum(table_name, current_rows);
        let do_analyze = self.needs_analyze(table_name, current_rows);

        let act = self.get_or_create_activity(table_name);
        let now_millis = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        if do_vacuum {
            let map_name = table_def.map_name();
            let _pruned = tx_store.vacuum_map(&map_name)?;
            act.dead_tuple_count.store(0, Ordering::SeqCst);
            act.last_vacuum_time.store(now_millis, Ordering::SeqCst);
        }

        if do_analyze {
            let tx = tx_store.begin();
            let (stats, col_stats) = analyze_table_with_sample(&tx, &table_def, SampleSpec::Default)?;
            let _ = tx.commit();
            catalog.update_table_stats(table_name, stats, col_stats)?;
            act.mod_count.store(0, Ordering::SeqCst);
            act.last_analyze_time.store(now_millis, Ordering::SeqCst);
        }

        Ok((do_vacuum, do_analyze))
    }

    /// 全テーブルに対して必要に応じ AutoVacuum / AutoAnalyze を実行
    pub fn run_all_maintenance(
        &self,
        tx_store: &Arc<TransactionStore>,
        catalog: &Arc<Catalog>,
    ) -> H2Result<Vec<(String, bool, bool)>> {
        let tables = catalog.all_tables();
        let mut results = Vec::new();
        for table in tables {
            let (vac, ana) = self.run_maintenance_for_table(tx_store, catalog, &table.name)?;
            if vac || ana {
                results.push((table.name, vac, ana));
            }
        }
        Ok(results)
    }
}
