use sqlparser::ast::{Expr, Query, SelectItem, SetExpr, Statement, TableFactor};
use std::sync::Arc;

use h2_mvstore::{MVStore, Transaction, TransactionStore};
use h2_types::{H2Error, H2Result, Value};
use crate::catalog::Catalog;
use crate::expression::{evaluate_expr, evaluate_sql_value};
use crate::parser::{extract_create_table, parse_sql};
use crate::row::Row;

#[derive(Debug, Clone)]
pub enum ExecutionResult {
    Ddl,
    Dml { affected_rows: u64 },
    Query { columns: Vec<String>, rows: Vec<Row> },
}

pub struct SQLEngine {
    store: Arc<MVStore>,
    tx_store: Arc<TransactionStore>,
    catalog: Arc<Catalog>,
}

impl SQLEngine {
    pub fn new(store: Arc<MVStore>) -> H2Result<Self> {
        let tx_store = TransactionStore::new(Arc::clone(&store));
        let catalog = Arc::new(Catalog::new(Arc::clone(&store))?);
        Ok(Self {
            store,
            tx_store,
            catalog,
        })
    }

    pub fn tx_store(&self) -> &Arc<TransactionStore> {
        &self.tx_store
    }

    pub fn catalog(&self) -> &Arc<Catalog> {
        &self.catalog
    }

    /// 暗黙トランザクション（Auto-commit）でSQLを実行
    pub fn execute(&self, sql: &str) -> H2Result<ExecutionResult> {
        let tx = self.tx_store.begin();
        let result = self.execute_with_tx(&tx, sql)?;
        tx.commit()?;
        Ok(result)
    }

    /// 明示的トランザクションコンテキストでSQLを実行
    pub fn execute_with_tx(&self, tx: &Transaction, sql: &str) -> H2Result<ExecutionResult> {
        let statements = parse_sql(sql)?;
        let mut last_result = ExecutionResult::Ddl;

        for stmt in statements {
            last_result = self.execute_statement(tx, stmt)?;
        }

        Ok(last_result)
    }

    fn execute_statement(&self, tx: &Transaction, stmt: Statement) -> H2Result<ExecutionResult> {
        match stmt {
            Statement::CreateTable(create_table) => {
                let table_def = extract_create_table(
                    &create_table.name,
                    &create_table.columns,
                    &create_table.constraints,
                )?;
                self.catalog.create_table(table_def)?;
                self.store.commit()?;
                Ok(ExecutionResult::Ddl)
            }
            Statement::Insert(insert) => {
                let table_name = insert.table_name.to_string();
                let table_def = self.catalog.get_table(&table_name).ok_or_else(|| {
                    H2Error::Catalog(format!("Table '{}' not found", table_name))
                })?;

                let map_name = format!("tbl_{}", table_name.to_lowercase());

                let mut affected_rows = 0;
                if let Some(source) = insert.source {
                    if let SetExpr::Values(values) = *source.body {
                        for row_exprs in values.rows {
                            if row_exprs.len() != table_def.columns.len() {
                                return Err(H2Error::Execution(format!(
                                    "Column count mismatch: expected {}, got {}",
                                    table_def.columns.len(),
                                    row_exprs.len()
                                )));
                            }

                            let mut row_values = Vec::with_capacity(row_exprs.len());
                            for expr in row_exprs {
                                match expr {
                                    Expr::Value(v) => row_values.push(evaluate_sql_value(&v)?),
                                    _ => return Err(H2Error::Execution("Expressions in VALUES not fully supported yet".to_string())),
                                }
                            }

                            let row_id = self.catalog.allocate_row_id(&table_name)?;
                            let row = Row::new(row_values);
                            tx.put(&map_name, row_id.to_le_bytes().to_vec(), row.to_bytes()?)?;
                            affected_rows += 1;
                        }
                    }
                }

                Ok(ExecutionResult::Dml { affected_rows })
            }
            Statement::Query(query) => self.execute_query(tx, *query),
            _ => Err(H2Error::Execution(format!("Unsupported statement: {:?}", stmt))),
        }
    }

    fn execute_query(&self, tx: &Transaction, query: Query) -> H2Result<ExecutionResult> {
        let SetExpr::Select(select) = *query.body else {
            return Err(H2Error::Execution("Only simple SELECT queries are supported".to_string()));
        };

        if select.from.is_empty() {
            return Err(H2Error::Execution("SELECT without FROM not supported yet".to_string()));
        }

        let from_table = &select.from[0];
        let table_name = match &from_table.relation {
            TableFactor::Table { name, .. } => name.to_string(),
            _ => return Err(H2Error::Execution("Complex table factors not supported".to_string())),
        };

        let table_def = self.catalog.get_table(&table_name).ok_or_else(|| {
            H2Error::Catalog(format!("Table '{}' not found", table_name))
        })?;

        let map_name = format!("tbl_{}", table_name.to_lowercase());

        // トランザクションのスナップショットに可視なエントリのみ走査
        let entries = tx.scan_visible(&map_name)?;
        let mut matched_rows = Vec::new();

        for (_key, val_bytes) in entries {
            let row = Row::from_bytes(&val_bytes)?;

            // WHERE 句のフィルタリング
            let matches = if let Some(selection) = &select.selection {
                match evaluate_expr(selection, &table_def, &row)? {
                    Value::Boolean(b) => b,
                    _ => false,
                }
            } else {
                true
            };

            if matches {
                matched_rows.push(row);
            }
        }

        // プロジェクション（SELECT カラムの抽出）
        let mut result_columns = Vec::new();
        let mut projected_rows = Vec::new();

        let is_wildcard = select.projection.iter().any(|p| matches!(p, SelectItem::Wildcard(_)));

        if is_wildcard {
            for col in &table_def.columns {
                result_columns.push(col.name.clone());
            }
            projected_rows = matched_rows;
        } else {
            let mut indices = Vec::new();
            for item in &select.projection {
                match item {
                    SelectItem::UnnamedExpr(Expr::Identifier(ident)) => {
                        let idx = table_def.column_index(&ident.value).ok_or_else(|| {
                            H2Error::Execution(format!("Column '{}' not found", ident.value))
                        })?;
                        indices.push(idx);
                        result_columns.push(ident.value.clone());
                    }
                    _ => return Err(H2Error::Execution("Complex select items not supported yet".to_string())),
                }
            }

            for row in matched_rows {
                let mut new_vals = Vec::with_capacity(indices.len());
                for &idx in &indices {
                    new_vals.push(row.get(idx).cloned().unwrap_or(Value::Null));
                }
                projected_rows.push(Row::new(new_vals));
            }
        }

        Ok(ExecutionResult::Query {
            columns: result_columns,
            rows: projected_rows,
        })
    }
}
