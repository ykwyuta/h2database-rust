use std::collections::HashMap;
use sqlparser::ast::{
    Statement, TableFactor,
};
use h2_mvstore::Transaction;
use h2_types::{H2Error, H2Result, Value};
use crate::catalog::{ColumnDef, TableDef};
use crate::row::Row;
use super::*;

impl SQLEngine {
    pub(crate) fn resolve_view_query(

        &self,
        tx: &Transaction,
        view: &crate::catalog::ViewDef,
        alias: Option<String>,
        current_ctes: &HashMap<String, (TableDef, Vec<Row>)>,
    ) -> H2Result<(TableDef, Vec<Row>, Option<String>)> {
        let dialect = sqlparser::dialect::GenericDialect {};
        let ast = sqlparser::parser::Parser::parse_sql(&dialect, &view.query_sql)
            .map_err(|e| H2Error::SqlParse(e.to_string()))?;
        if let Some(Statement::Query(view_q)) = ast.into_iter().next() {
            let sub_res = self.execute_query_with_ctes(tx, *view_q, current_ctes)?;
            let (cols, rows) = match sub_res {
                ExecutionResult::Query { columns, rows } => (columns, rows),
                _ => return Err(H2Error::Execution("View query must return rows".to_string())),
            };
            let final_cols = if !view.columns.is_empty() && view.columns.len() == cols.len() {
                view.columns.clone()
            } else {
                cols
            };
            let col_defs = final_cols.into_iter().map(|c| ColumnDef::new(
                c,
                h2_types::DataType::VarChar(None),
                true,
                false,
            )).collect();
            let effective_name = alias.clone().unwrap_or_else(|| view.name.clone());
            let t_def = crate::catalog::TableDef::new(effective_name, col_defs);
            Ok((t_def, rows, alias))
        } else {
            Err(H2Error::Execution(format!("Invalid query in view '{}'", view.name)))
        }
    }

    pub(crate) fn resolve_virtual_graph_table(
        &self,
        tx: &Transaction,
        table_name: &str,
        table_alias: Option<String>,
    ) -> H2Result<Option<(TableDef, Vec<Row>, Option<String>)>> {
        let (graph_name, kind) = match crate::catalog::parse_virtual_graph_table(table_name) {
            Some(res) => res,
            None => return Ok(None),
        };

        let mut table_def = match self.catalog.get_table(table_name) {
            Some(t) => t,
            None => return Ok(None),
        };

        if let Some(ref a) = table_alias {
            table_def.name = a.clone();
        }

        let rows = match kind {
            crate::catalog::VirtualGraphKind::Nodes => {
                let map_name = format!("_g_{}_nodes", graph_name);
                let entries = tx.scan_visible(&map_name)?;
                let mut rows = Vec::with_capacity(entries.len());
                for (_k, val_bytes) in entries {
                    if let Ok(node) = bincode::deserialize::<h2_graph::Node>(&val_bytes) {
                        let labels_val = Value::Array(node.labels.into_iter().map(Value::String).collect());
                        let mut prop_map = serde_json::Map::new();
                        for (k, v) in node.properties {
                            prop_map.insert(k, serde_json::to_value(&v).unwrap_or(serde_json::Value::Null));
                        }
                        let prop_val = Value::Json(serde_json::Value::Object(prop_map));
                        rows.push(Row::new(vec![
                            Value::BigInt(node.id as i64),
                            labels_val,
                            prop_val,
                        ]));
                    }
                }
                rows
            }
            crate::catalog::VirtualGraphKind::Edges => {
                let map_name = format!("_g_{}_edges", graph_name);
                let entries = tx.scan_visible(&map_name)?;
                let mut rows = Vec::with_capacity(entries.len());
                for (_k, val_bytes) in entries {
                    if let Ok(edge) = bincode::deserialize::<h2_graph::Edge>(&val_bytes) {
                        let mut prop_map = serde_json::Map::new();
                        for (k, v) in edge.properties {
                            prop_map.insert(k, serde_json::to_value(&v).unwrap_or(serde_json::Value::Null));
                        }
                        let prop_val = Value::Json(serde_json::Value::Object(prop_map));
                        rows.push(Row::new(vec![
                            Value::BigInt(edge.id as i64),
                            Value::BigInt(edge.src_id as i64),
                            Value::BigInt(edge.dst_id as i64),
                            Value::String(edge.edge_type),
                            prop_val,
                        ]));
                    }
                }
                rows
            }
        };

        Ok(Some((table_def, rows, table_alias)))
    }

    pub(crate) fn resolve_cypher_table_function(
        &self,
        tx: &Transaction,
        name: &sqlparser::ast::ObjectName,
        alias: &Option<sqlparser::ast::TableAlias>,
        args: &Option<sqlparser::ast::TableFunctionArgs>,
    ) -> H2Result<Option<(TableDef, Vec<Row>, Option<String>)>> {
        let func_name = name.to_string();
        if !func_name.eq_ignore_ascii_case("cypher") {
            return Ok(None);
        }

        let func_args = match args {
            Some(fa) => &fa.args,
            None => {
                return Err(H2Error::Execution(
                    "CYPHER() table function requires at least 2 arguments: graph_name and cypher_query".to_string(),
                ))
            }
        };

        if func_args.len() < 2 {
            return Err(H2Error::Execution(
                "CYPHER() table function requires at least 2 arguments: graph_name and cypher_query".to_string(),
            ));
        }

        fn extract_str_arg(arg: &sqlparser::ast::FunctionArg) -> H2Result<String> {
            match arg {
                sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(expr)) => {
                    match expr {
                        sqlparser::ast::Expr::Value(sqlparser::ast::Value::SingleQuotedString(s)) => {
                            Ok(s.clone())
                        }
                        sqlparser::ast::Expr::Value(sqlparser::ast::Value::DoubleQuotedString(s)) => {
                            Ok(s.clone())
                        }
                        sqlparser::ast::Expr::Identifier(ident) => Ok(ident.value.clone()),
                        _ => Err(H2Error::Execution(format!(
                            "Expected string literal argument to CYPHER(), found {:?}",
                            expr
                        ))),
                    }
                }
                _ => Err(H2Error::Execution(
                    "Expected unnamed string argument to CYPHER()".to_string(),
                )),
            }
        }

        let graph_name = extract_str_arg(&func_args[0])?;
        let cypher_query = extract_str_arg(&func_args[1])?;

        let mut params = HashMap::new();
        if func_args.len() >= 3 {
            if let Ok(params_str) = extract_str_arg(&func_args[2]) {
                if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(&params_str) {
                    if let serde_json::Value::Object(map) = json_val {
                        for (k, v) in map {
                            let gv = match v {
                                serde_json::Value::Null => h2_graph::GraphValue::Null,
                                serde_json::Value::Bool(b) => h2_graph::GraphValue::Boolean(b),
                                serde_json::Value::Number(n) => {
                                    if let Some(i) = n.as_i64() {
                                        h2_graph::GraphValue::Integer(i)
                                    } else {
                                        h2_graph::GraphValue::Float(n.as_f64().unwrap_or(0.0))
                                    }
                                }
                                serde_json::Value::String(s) => h2_graph::GraphValue::String(s),
                                _ => h2_graph::GraphValue::String(v.to_string()),
                            };
                            params.insert(k, gv);
                        }
                    }
                }
            }
        }

        let engine = h2_graph::GraphEngine::new(self.store.clone(), &graph_name)?;
        let result = engine.execute_on_tx(tx, &cypher_query, &params)?;

        let table_alias_name = alias
            .as_ref()
            .map(|a| a.name.value.clone())
            .unwrap_or_else(|| "cypher".to_string());

        let col_names = if let Some(ref a) = alias {
            if !a.columns.is_empty() {
                a.columns.iter().map(|c| c.name.value.clone()).collect()
            } else {
                result.columns.clone()
            }
        } else {
            result.columns.clone()
        };

        let col_defs: Vec<ColumnDef> = col_names
            .into_iter()
            .map(|col_name| ColumnDef::new(col_name, h2_types::DataType::VarChar(None), true, false))
            .collect();

        let mut rows = Vec::with_capacity(result.rows.len());
        for row in result.rows {
            let row_values: Vec<Value> = row.into_iter().map(|gv| gv.to_value()).collect();
            rows.push(Row::new(row_values));
        }

        let table_def = TableDef::new(table_alias_name.clone(), col_defs);
        Ok(Some((table_def, rows, Some(table_alias_name))))
    }

    pub(crate) fn resolve_iceberg_table_function(
        &self,
        name: &sqlparser::ast::ObjectName,
        alias: &Option<sqlparser::ast::TableAlias>,
        args: &Option<sqlparser::ast::TableFunctionArgs>,
    ) -> H2Result<Option<(TableDef, Vec<Row>, Option<String>)>> {
        let func_name = name.to_string();
        let is_snapshots = func_name.eq_ignore_ascii_case("iceberg_snapshots");
        let is_files = func_name.eq_ignore_ascii_case("iceberg_files");
        let is_scan = func_name.eq_ignore_ascii_case("iceberg_scan");
        let is_compact = func_name.eq_ignore_ascii_case("iceberg_compact");

        if !is_snapshots && !is_files && !is_scan && !is_compact {
            return Ok(None);
        }

        let func_args = match args {
            Some(fa) => &fa.args,
            None => {
                return Err(H2Error::Execution(format!(
                    "{}() requires a table name argument",
                    func_name
                )))
            }
        };

        if func_args.is_empty() {
            return Err(H2Error::Execution(format!(
                "{}() requires a table name argument",
                func_name
            )));
        }

        let target_table_name = match &func_args[0] {
            sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(expr)) => {
                match expr {
                    sqlparser::ast::Expr::Value(sqlparser::ast::Value::SingleQuotedString(s))
                    | sqlparser::ast::Expr::Value(sqlparser::ast::Value::DoubleQuotedString(s)) => s.clone(),
                    sqlparser::ast::Expr::Identifier(ident) => ident.value.clone(),
                    _ => return Err(H2Error::Execution("Expected table name string argument".to_string())),
                }
            }
            _ => return Err(H2Error::Execution("Expected unnamed table name argument".to_string())),
        };

        let target_def = self.catalog.get_table(&target_table_name).ok_or_else(|| {
            H2Error::Catalog(format!("Table '{}' not found", target_table_name))
        })?;

        if !target_def.is_iceberg {
            return Err(H2Error::Execution(format!("Table '{}' is not an Iceberg table", target_table_name)));
        }

        let location = target_def.iceberg_location.as_deref().unwrap_or("");
        let table_alias_name = alias.as_ref().map(|a| a.name.value.clone()).unwrap_or_else(|| func_name.clone());

        if is_snapshots {
            let t_def = TableDef::new(
                table_alias_name.clone(),
                vec![
                    ColumnDef::new("snapshot_id", h2_types::DataType::BigInt, false, true),
                    ColumnDef::new("parent_snapshot_id", h2_types::DataType::BigInt, true, false),
                    ColumnDef::new("timestamp_ms", h2_types::DataType::BigInt, false, false),
                    ColumnDef::new("operation", h2_types::DataType::VarChar(None), false, false),
                    ColumnDef::new("added_records", h2_types::DataType::BigInt, false, false),
                    ColumnDef::new("total_records", h2_types::DataType::BigInt, false, false),
                    ColumnDef::new("manifest_list", h2_types::DataType::VarChar(None), false, false),
                ],
            );
            let rows = crate::iceberg::get_iceberg_snapshots(location)?;
            Ok(Some((t_def, rows, Some(table_alias_name))))
        } else if is_files {
            let t_def = TableDef::new(
                table_alias_name.clone(),
                vec![
                    ColumnDef::new("snapshot_id", h2_types::DataType::BigInt, false, false),
                    ColumnDef::new("file_path", h2_types::DataType::VarChar(None), false, true),
                    ColumnDef::new("file_format", h2_types::DataType::VarChar(None), false, false),
                    ColumnDef::new("record_count", h2_types::DataType::BigInt, false, false),
                    ColumnDef::new("file_size_in_bytes", h2_types::DataType::BigInt, false, false),
                    ColumnDef::new("lower_bounds", h2_types::DataType::VarChar(None), true, false),
                    ColumnDef::new("upper_bounds", h2_types::DataType::VarChar(None), true, false),
                ],
            );
            let rows = crate::iceberg::get_iceberg_files(location)?;
            Ok(Some((t_def, rows, Some(table_alias_name))))
        } else if is_scan {
            let mut snapshot_id = None;
            if func_args.len() >= 2 {
                if let sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(expr)) = &func_args[1] {
                    match expr {
                        sqlparser::ast::Expr::Value(sqlparser::ast::Value::Number(n, _)) => {
                            snapshot_id = n.parse::<i64>().ok();
                        }
                        sqlparser::ast::Expr::Value(sqlparser::ast::Value::SingleQuotedString(s)) => {
                            snapshot_id = s.parse::<i64>().ok();
                        }
                        _ => {}
                    }
                }
            }
            let rows = crate::iceberg::read_iceberg_table_rows(&target_def, None, snapshot_id)?;
            let mut t_def = target_def;
            t_def.name = table_alias_name.clone();
            Ok(Some((t_def, rows, Some(table_alias_name))))
        } else {
            let res = crate::iceberg::compact_iceberg_table(&target_def)?;
            let t_def = TableDef::new(
                table_alias_name.clone(),
                vec![
                    ColumnDef::new("table_name", h2_types::DataType::VarChar(None), false, true),
                    ColumnDef::new("compacted", h2_types::DataType::Boolean, false, false),
                    ColumnDef::new("files_before", h2_types::DataType::BigInt, false, false),
                    ColumnDef::new("files_after", h2_types::DataType::BigInt, false, false),
                    ColumnDef::new("total_records", h2_types::DataType::BigInt, false, false),
                    ColumnDef::new("delete_files_removed", h2_types::DataType::BigInt, false, false),
                ],
            );
            let rows = vec![Row::new(vec![
                Value::String(res.table_name),
                Value::Boolean(res.compacted),
                Value::BigInt(res.files_before as i64),
                Value::BigInt(res.files_after as i64),
                Value::BigInt(res.total_records as i64),
                Value::BigInt(res.delete_files_removed as i64),
            ])];
            Ok(Some((t_def, rows, Some(table_alias_name))))
        }
    }

    pub(crate) fn resolve_pg_proc_table(
        &self,
        table_name: &str,
        table_alias: Option<String>,
    ) -> (crate::catalog::TableDef, Vec<Row>, Option<String>) {
        let t_def = crate::catalog::TableDef::new(
            table_name.to_string(),
            vec![
                crate::catalog::ColumnDef::new("oid", h2_types::DataType::BigInt, false, true),
                crate::catalog::ColumnDef::new("proname", h2_types::DataType::VarChar(None), false, false),
                crate::catalog::ColumnDef::new("prokind", h2_types::DataType::VarChar(None), false, false),
                crate::catalog::ColumnDef::new("prorettype", h2_types::DataType::VarChar(None), false, false),
                crate::catalog::ColumnDef::new("pronargs", h2_types::DataType::Integer, false, false),
                crate::catalog::ColumnDef::new("proargnames", h2_types::DataType::VarChar(None), false, false),
                crate::catalog::ColumnDef::new("prosrc", h2_types::DataType::VarChar(None), false, false),
            ],
        );

        let mut rows = Vec::new();
        let mut oid = 10001i64;

        // 登録済みユーザー定義ルーチン
        for r in self.catalog.all_routines() {
            let kind_str = match r.kind {
                crate::procedural::RoutineKind::Function => "f",
                crate::procedural::RoutineKind::Procedure => "p",
            };
            let ret_str = r.return_type.map(|t| t.to_string().to_lowercase()).unwrap_or_else(|| "void".to_string());
            let arg_names = r.parameters.iter().map(|p| p.name.clone()).collect::<Vec<_>>().join(",");

            rows.push(Row::new(vec![
                Value::BigInt(oid),
                Value::String(r.name),
                Value::String(kind_str.to_string()),
                Value::String(ret_str),
                Value::Integer(r.parameters.len() as i32),
                Value::String(arg_names),
                Value::String(r.source_sql),
            ]));
            oid += 1;
        }

        (t_def, rows, table_alias)
    }

    pub(crate) fn resolve_table_factor(
        &self,
        tx: &Transaction,
        relation: &TableFactor,
        current_ctes: &HashMap<String, (TableDef, Vec<Row>)>,
    ) -> H2Result<(TableDef, Vec<Row>, Option<String>)> {
        match relation {
            TableFactor::Derived { subquery, alias, .. } => {
                let sub_alias = alias.as_ref().map(|a| a.name.value.clone()).unwrap_or_else(|| "subquery".to_string());
                let sub_res = self.execute_query_with_ctes(tx, *subquery.clone(), current_ctes)?;
                let (cols, rows) = match sub_res {
                    ExecutionResult::Query { columns, rows } => (columns, rows),
                    _ => return Err(H2Error::Execution("Derived table must be a query".to_string())),
                };
                let col_defs = cols.into_iter().map(|c| ColumnDef::new(
                    c,
                    h2_types::DataType::VarChar(None),
                    true,
                    false,
                )).collect();
                let t_def = crate::catalog::TableDef::new(sub_alias.clone(), col_defs);
                Ok((t_def, rows, Some(sub_alias)))
            }
            TableFactor::Table { name, alias, args, .. } => {
                if let Some(res) = self.resolve_cypher_table_function(tx, name, alias, args)? {
                    return Ok(res);
                }
                if let Some(res) = self.resolve_iceberg_table_function(name, alias, args)? {
                    return Ok(res);
                }
                let table_name = normalize_object_name(name);
                let table_alias = alias.as_ref().map(|a| a.name.value.clone());

                if let Some(res) = self.resolve_virtual_graph_table(tx, &table_name, table_alias.clone())? {
                    return Ok(res);
                }

                if let Some((cte_def, cte_rows)) = current_ctes.get(&table_name.to_lowercase()) {
                    let mut t_def = cte_def.clone();
                    if let Some(ref a) = table_alias {
                        t_def.name = a.clone();
                    }
                    Ok((t_def, cte_rows.clone(), table_alias))
                } else if let Some(view) = self.catalog.get_view(&table_name) {
                    self.resolve_view_query(tx, &view, table_alias, current_ctes)
                } else {
                    let is_pg_proc = table_name.eq_ignore_ascii_case("pg_proc")
                        || table_name.eq_ignore_ascii_case("pg_catalog.pg_proc");
                    if is_pg_proc {
                        return Ok(self.resolve_pg_proc_table(&table_name, table_alias));
                    }

                    if table_name.eq_ignore_ascii_case("dual") {
                        let t_def = crate::catalog::TableDef::new(
                            table_name.clone(),
                            vec![
                                crate::catalog::ColumnDef::new("dummy", h2_types::DataType::VarChar(Some(1)), false, false),
                            ],
                        );
                        let rows = vec![Row::new(vec![Value::String("X".to_string())])];
                        return Ok((t_def, rows, table_alias));
                    }

                    let is_info_tables = table_name.eq_ignore_ascii_case("information_schema.tables")
                        || table_name.eq_ignore_ascii_case("tables");
                    let is_info_columns = table_name.eq_ignore_ascii_case("information_schema.columns")
                        || table_name.eq_ignore_ascii_case("columns");
                    let is_info_schemata = table_name.eq_ignore_ascii_case("information_schema.schemata")
                        || table_name.eq_ignore_ascii_case("schemata");

                    if is_info_tables {
                        let t_def = crate::catalog::TableDef::new(
                            table_name.clone(),
                            vec![
                                crate::catalog::ColumnDef::new("table_name", h2_types::DataType::VarChar(None), false, true),
                                crate::catalog::ColumnDef::new("columns_count", h2_types::DataType::Integer, false, false),
                            ],
                        );
                        let tables = self.catalog.all_tables();
                        let rows = tables.into_iter().map(|t| Row::new(vec![
                            Value::String(t.name),
                            Value::Integer(t.columns.len() as i32),
                        ])).collect();
                        Ok((t_def, rows, table_alias))
                    } else if is_info_columns {
                        let t_def = crate::catalog::TableDef::new(
                            table_name.clone(),
                            vec![
                                crate::catalog::ColumnDef::new("table_name", h2_types::DataType::VarChar(None), false, true),
                                crate::catalog::ColumnDef::new("column_name", h2_types::DataType::VarChar(None), false, false),
                                crate::catalog::ColumnDef::new("data_type", h2_types::DataType::VarChar(None), false, false),
                                crate::catalog::ColumnDef::new("is_nullable", h2_types::DataType::Boolean, false, false),
                            ],
                        );
                        let tables = self.catalog.all_tables();
                        let mut rows = Vec::new();
                        for t in tables {
                            for c in &t.columns {
                                rows.push(Row::new(vec![
                                    Value::String(t.name.clone()),
                                    Value::String(c.name.clone()),
                                    Value::String(c.data_type.to_string()),
                                    Value::Boolean(c.is_nullable),
                                ]));
                            }
                        }
                        Ok((t_def, rows, table_alias))
                    } else if is_info_schemata {
                        let t_def = crate::catalog::TableDef::new(
                            table_name.clone(),
                            vec![
                                crate::catalog::ColumnDef::new("schema_name", h2_types::DataType::VarChar(None), false, true),
                            ],
                        );
                        let mut schemas = self.catalog.get_schemas();
                        schemas.sort();
                        let rows = schemas.into_iter().map(|s| Row::new(vec![Value::String(s)])).collect();
                        Ok((t_def, rows, table_alias))
                    } else {
                        let table_def = self.catalog.get_table(&table_name).ok_or_else(|| {
                            H2Error::Catalog(format!("Table '{}' not found", table_name))
                        })?;
                        if table_def.is_iceberg {
                            let rows = crate::iceberg::read_iceberg_table_rows(&table_def, None, None)?;
                            return Ok((table_def, rows, table_alias));
                        }
                        let map_name = table_def.map_name();
                        let entries = tx.scan_visible(&map_name)?;
                        let mut rows = Vec::with_capacity(entries.len());
                        let now_ms = chrono::Utc::now().timestamp_millis();
                        for (_k, val_bytes) in entries {
                            let mut row = Row::from_bytes(&val_bytes)?;
                            table_def.align_row(&mut row);
                            if table_def.is_cache {
                                if let Some(Value::BigInt(exp)) = row.values.get(0) {
                                    if *exp <= now_ms {
                                        continue;
                                    }
                                }
                            }
                            rows.push(row);
                        }
                        Ok((table_def, rows, table_alias))
                    }
                }
            }
            _ => Err(H2Error::Execution("Complex table factors not supported".to_string())),
        }
    }

}
