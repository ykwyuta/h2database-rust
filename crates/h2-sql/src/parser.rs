use sqlparser::ast::{
    CharacterLength, ColumnDef as SqlColumnDef, DataType as SqlDataType, Statement, TableConstraint,
};
use sqlparser::dialect::{AnsiDialect, GenericDialect, MsSqlDialect, MySqlDialect, PostgreSqlDialect};
use sqlparser::parser::Parser;

use h2_types::{DataType, H2Error, H2Result, SqlDialectMode};
use crate::catalog::{ColumnDef, ForeignKeyAction, ForeignKeyDef, TableDef, UniqueConstraintDef};

pub fn parse_sql(sql: &str) -> H2Result<Vec<Statement>> {
    parse_sql_mode(sql, SqlDialectMode::Regular)
}

pub fn parse_sql_mode(sql: &str, mode: SqlDialectMode) -> H2Result<Vec<Statement>> {
    match mode {
        SqlDialectMode::PostgreSql => {
            let dialect = PostgreSqlDialect {};
            Parser::parse_sql(&dialect, sql)
                .map_err(|e| H2Error::SqlParse(e.to_string()))
        }
        SqlDialectMode::MySql => {
            let dialect = MySqlDialect {};
            Parser::parse_sql(&dialect, sql)
                .map_err(|e| H2Error::SqlParse(e.to_string()))
        }
        SqlDialectMode::MsSqlServer => {
            let dialect = MsSqlDialect {};
            Parser::parse_sql(&dialect, sql)
                .map_err(|e| H2Error::SqlParse(e.to_string()))
        }
        SqlDialectMode::Db2 => {
            let dialect = AnsiDialect {};
            Parser::parse_sql(&dialect, sql)
                .map_err(|e| H2Error::SqlParse(e.to_string()))
        }
        SqlDialectMode::Regular | SqlDialectMode::Oracle => {
            let generic = GenericDialect {};
            if let Ok(stmts) = Parser::parse_sql(&generic, sql) {
                return Ok(stmts);
            }
            let pg = PostgreSqlDialect {};
            if let Ok(stmts) = Parser::parse_sql(&pg, sql) {
                return Ok(stmts);
            }
            let mysql = MySqlDialect {};
            Parser::parse_sql(&mysql, sql)
                .map_err(|e| H2Error::SqlParse(e.to_string()))
        }
    }
}

fn char_len_to_usize(len: &Option<CharacterLength>) -> Option<usize> {
    len.as_ref().and_then(|cl| match cl {
        CharacterLength::IntegerLength { length, .. } => Some(*length as usize),
        CharacterLength::Max => None,
    })
}

/// SQL の DataType を h2-types の DataType へ変換（多方言対応）
pub fn convert_data_type(sql_type: &SqlDataType) -> H2Result<DataType> {
    match sql_type {
        SqlDataType::Boolean => Ok(DataType::Boolean),
        SqlDataType::TinyInt(_) => Ok(DataType::TinyInt),
        SqlDataType::SmallInt(_) | SqlDataType::Int2(_) => Ok(DataType::SmallInt),
        SqlDataType::Int(_) | SqlDataType::Integer(_) | SqlDataType::Int4(_) => Ok(DataType::Integer),
        SqlDataType::BigInt(_) | SqlDataType::Int8(_) | SqlDataType::Int64 => Ok(DataType::BigInt),
        SqlDataType::Float(_) | SqlDataType::Real | SqlDataType::Float4 => Ok(DataType::Float),
        SqlDataType::Double | SqlDataType::DoublePrecision | SqlDataType::Float8 => Ok(DataType::Double),
        SqlDataType::Bytea => Ok(DataType::Binary(None)),
        SqlDataType::Decimal(exact_info) | SqlDataType::Numeric(exact_info) => {
            match exact_info {
                sqlparser::ast::ExactNumberInfo::PrecisionAndScale(p, s) => {
                    Ok(DataType::Decimal(*p as u8, *s as u8))
                }
                sqlparser::ast::ExactNumberInfo::Precision(p) => {
                    Ok(DataType::Decimal(*p as u8, 0))
                }
                sqlparser::ast::ExactNumberInfo::None => Ok(DataType::Decimal(10, 2)),
            }
        }
        SqlDataType::Char(len) => Ok(DataType::Char(char_len_to_usize(len).unwrap_or(1))),
        SqlDataType::Varchar(len) => Ok(DataType::VarChar(char_len_to_usize(len))),
        SqlDataType::Nvarchar(len) => Ok(DataType::VarChar(char_len_to_usize(len))),
        SqlDataType::Text => Ok(DataType::VarChar(None)),
        SqlDataType::Binary(len) => Ok(DataType::Binary(len.map(|l| l as usize))),
        SqlDataType::Varbinary(len) => Ok(DataType::Binary(len.map(|l| l as usize))),
        SqlDataType::Blob(_) | SqlDataType::Clob(_) => Ok(DataType::Blob),
        SqlDataType::Date => Ok(DataType::Date),
        SqlDataType::Time(_, _) => Ok(DataType::Time),
        SqlDataType::Datetime(_) => Ok(DataType::Timestamp),
        SqlDataType::Timestamp(_, tz) => {
            if matches!(tz, sqlparser::ast::TimezoneInfo::WithTimeZone) {
                Ok(DataType::TimestampTz)
            } else {
                Ok(DataType::Timestamp)
            }
        }
        SqlDataType::MediumInt(_) => Ok(DataType::Integer),
        SqlDataType::Uuid => Ok(DataType::Uuid),
        SqlDataType::JSON => Ok(DataType::Json),
        SqlDataType::Interval => Ok(DataType::Interval),
        SqlDataType::Custom(name, modifiers) => {
            let type_name = name.to_string().to_uppercase();
            if type_name == "TIMESTAMPTZ" {
                Ok(DataType::TimestampTz)
            } else if type_name == "INTERVAL" {
                Ok(DataType::Interval)
            } else if type_name == "SERIAL" {
                Ok(DataType::Integer)
            } else if type_name == "BIGSERIAL" {
                Ok(DataType::BigInt)
            } else if type_name == "VARCHAR2" || type_name == "NVARCHAR2" {
                let len = modifiers.first().and_then(|s| s.parse::<usize>().ok());
                Ok(DataType::VarChar(len))
            } else if type_name == "NCHAR" {
                let len = modifiers.first().and_then(|s| s.parse::<usize>().ok()).unwrap_or(1);
                Ok(DataType::Char(len))
            } else if type_name == "NUMBER" {
                if let Some(p) = modifiers.first().and_then(|s| s.parse::<u8>().ok()) {
                    let s = modifiers.get(1).and_then(|s| s.parse::<u8>().ok()).unwrap_or(0);
                    Ok(DataType::Decimal(p, s))
                } else {
                    Ok(DataType::Decimal(10, 2))
                }
            } else if type_name == "RAW" {
                let len = modifiers.first().and_then(|s| s.parse::<usize>().ok());
                Ok(DataType::Binary(len))
            } else if type_name == "CLOB" || type_name == "LONGTEXT" || type_name == "MEDIUMTEXT" || type_name == "TINYTEXT" || type_name == "NTEXT" {
                Ok(DataType::VarChar(None))
            } else if type_name == "BLOB" || type_name == "LONGBLOB" || type_name == "MEDIUMBLOB" || type_name == "TINYBLOB" || type_name == "IMAGE" {
                Ok(DataType::Blob)
            } else if type_name == "DATETIME" || type_name == "DATETIME2" || type_name == "SMALLDATETIME" {
                Ok(DataType::Timestamp)
            } else if type_name == "MONEY" || type_name == "SMALLMONEY" {
                Ok(DataType::Decimal(19, 4))
            } else if type_name == "BIT" {
                Ok(DataType::Boolean)
            } else if type_name == "UNIQUEIDENTIFIER" {
                Ok(DataType::Uuid)
            } else {
                Err(H2Error::TypeError(format!("Unsupported custom type: {}", type_name)))
            }
        }
        _ => Err(H2Error::TypeError(format!("Unsupported SQL data type: {:?}", sql_type))),
    }
}

fn convert_referential_action(action: &Option<sqlparser::ast::ReferentialAction>) -> ForeignKeyAction {
    match action {
        Some(sqlparser::ast::ReferentialAction::Cascade) => ForeignKeyAction::Cascade,
        Some(sqlparser::ast::ReferentialAction::SetNull) => ForeignKeyAction::SetNull,
        Some(sqlparser::ast::ReferentialAction::Restrict) => ForeignKeyAction::Restrict,
        _ => ForeignKeyAction::NoAction,
    }
}

/// CREATE TABLE 文から TableDef を抽出
pub fn extract_create_table(
    name: &sqlparser::ast::ObjectName,
    columns: &[SqlColumnDef],
    constraints: &[TableConstraint],
) -> H2Result<TableDef> {
    let (schema_name, table_name) = match name.0.len() {
        2 => (name.0[0].value.clone(), name.0[1].value.clone()),
        _ => (
            "public".to_string(),
            name.0.last().map(|i| i.value.clone()).unwrap_or_else(|| name.to_string()),
        ),
    };
    let mut col_defs = Vec::new();
    let mut pk_columns: Vec<String> = Vec::new();
    let mut unique_constraints = Vec::new();
    let mut foreign_keys = Vec::new();

    for constraint in constraints {
        match constraint {
            TableConstraint::PrimaryKey { columns, .. } => {
                for col in columns {
                    if !pk_columns.iter().any(|p| p.eq_ignore_ascii_case(&col.value)) {
                        pk_columns.push(col.value.clone());
                    }
                }
            }
            TableConstraint::Unique { name, columns, .. } => {
                let cols: Vec<String> = columns.iter().map(|c| c.value.clone()).collect();
                if !cols.is_empty() {
                    unique_constraints.push(UniqueConstraintDef {
                        name: name.as_ref().map(|n| n.value.clone()),
                        columns: cols,
                    });
                }
            }
            TableConstraint::ForeignKey { name, columns, foreign_table, referred_columns, on_delete, on_update, .. } => {
                if let (Some(col), Some(ref_col)) = (columns.first(), referred_columns.first()) {
                    foreign_keys.push(ForeignKeyDef {
                        name: name.as_ref().map(|n| n.value.clone()),
                        column: col.value.clone(),
                        foreign_table: foreign_table.to_string(),
                        foreign_column: ref_col.value.clone(),
                        on_delete: convert_referential_action(on_delete),
                        on_update: convert_referential_action(on_update),
                    });
                }
            }
            _ => {}
        }
    }

    for col in columns {
        let col_name = col.name.value.clone();
        let dt = convert_data_type(&col.data_type)?;
        let mut is_pk = pk_columns.iter().any(|pk| pk.eq_ignore_ascii_case(&col_name));
        let mut is_nullable = true;

        let is_custom_serial = match &col.data_type {
            SqlDataType::Custom(c_name, _) => {
                let upper = c_name.to_string().to_uppercase();
                upper == "SERIAL" || upper == "BIGSERIAL"
            }
            _ => false,
        };

        let mut has_identity = false;
        for opt in &col.options {
            match &opt.option {
                sqlparser::ast::ColumnOption::Generated { .. }
                | sqlparser::ast::ColumnOption::Identity(..) => {
                    has_identity = true;
                }
                sqlparser::ast::ColumnOption::Unique { is_primary, .. } => {
                    if *is_primary {
                        is_pk = true;
                        if !pk_columns.iter().any(|p| p.eq_ignore_ascii_case(&col_name)) {
                            pk_columns.push(col_name.clone());
                        }
                    } else {
                        unique_constraints.push(UniqueConstraintDef {
                            name: opt.name.as_ref().map(|n| n.value.clone()),
                            columns: vec![col_name.clone()],
                        });
                    }
                }
                sqlparser::ast::ColumnOption::NotNull => {
                    is_nullable = false;
                }
                sqlparser::ast::ColumnOption::ForeignKey { foreign_table, referred_columns, on_delete, on_update, .. } => {
                    let ref_col = referred_columns.first().map(|c| c.value.clone()).unwrap_or_else(|| "id".to_string());
                    foreign_keys.push(ForeignKeyDef {
                        name: opt.name.as_ref().map(|n| n.value.clone()),
                        column: col_name.clone(),
                        foreign_table: foreign_table.to_string(),
                        foreign_column: ref_col,
                        on_delete: convert_referential_action(on_delete),
                        on_update: convert_referential_action(on_update),
                    });
                }
                _ => {}
            }
        }

        let mut seq_name = None;
        if is_custom_serial || has_identity {
            seq_name = Some(format!("{}_{}_seq", table_name.to_lowercase(), col_name.to_lowercase()));
            is_nullable = false;
        }

        if is_pk {
            is_nullable = false;
        }

        let mut column_def = ColumnDef::new(col_name, dt, is_nullable, is_pk);
        column_def.sequence_name = seq_name;
        col_defs.push(column_def);
    }

    let mut t_def = TableDef::new(table_name, col_defs);
    t_def.schema = schema_name;
    t_def.foreign_keys = foreign_keys;
    t_def.primary_key = pk_columns;
    t_def.unique_constraints = unique_constraints;
    Ok(t_def)
}

