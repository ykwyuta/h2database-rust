use sqlparser::ast::{
    CharacterLength, ColumnDef as SqlColumnDef, DataType as SqlDataType, Statement, TableConstraint,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use h2_types::{DataType, H2Error, H2Result};
use crate::catalog::{ColumnDef, TableDef};

pub fn parse_sql(sql: &str) -> H2Result<Vec<Statement>> {
    let dialect = PostgreSqlDialect {};
    Parser::parse_sql(&dialect, sql)
        .map_err(|e| H2Error::SqlParse(e.to_string()))
}

fn char_len_to_usize(len: &Option<CharacterLength>) -> Option<usize> {
    len.as_ref().and_then(|cl| match cl {
        CharacterLength::IntegerLength { length, .. } => Some(*length as usize),
        CharacterLength::Max => None,
    })
}

/// SQL の DataType を h2-types の DataType へ変換
pub fn convert_data_type(sql_type: &SqlDataType) -> H2Result<DataType> {
    match sql_type {
        SqlDataType::Boolean => Ok(DataType::Boolean),
        SqlDataType::TinyInt(_) => Ok(DataType::TinyInt),
        SqlDataType::SmallInt(_) => Ok(DataType::SmallInt),
        SqlDataType::Int(_) | SqlDataType::Integer(_) => Ok(DataType::Integer),
        SqlDataType::BigInt(_) => Ok(DataType::BigInt),
        SqlDataType::Float(_) | SqlDataType::Real => Ok(DataType::Float),
        SqlDataType::Double | SqlDataType::DoublePrecision => Ok(DataType::Double),
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
        SqlDataType::Text => Ok(DataType::VarChar(None)),
        SqlDataType::Binary(len) => Ok(DataType::Binary(len.map(|l| l as usize))),
        SqlDataType::Blob(_) => Ok(DataType::Blob),
        SqlDataType::Date => Ok(DataType::Date),
        SqlDataType::Time(_, _) => Ok(DataType::Time),
        SqlDataType::Timestamp(_, tz) => {
            if matches!(tz, sqlparser::ast::TimezoneInfo::WithTimeZone) {
                Ok(DataType::TimestampTz)
            } else {
                Ok(DataType::Timestamp)
            }
        }
        SqlDataType::Uuid => Ok(DataType::Uuid),
        SqlDataType::JSON => Ok(DataType::Json),
        SqlDataType::Custom(name, args) => {
            let type_name = name.to_string().to_uppercase();
            if type_name == "VECTOR" {
                let dim = args.first().and_then(|a| a.parse::<usize>().ok()).unwrap_or(1536);
                Ok(DataType::Vector(dim))
            } else {
                Err(H2Error::TypeError(format!("Unsupported custom type: {}", type_name)))
            }
        }
        _ => Err(H2Error::TypeError(format!("Unsupported SQL data type: {:?}", sql_type))),
    }
}

/// CREATE TABLE 文から TableDef を抽出
pub fn extract_create_table(
    name: &sqlparser::ast::ObjectName,
    columns: &[SqlColumnDef],
    constraints: &[TableConstraint],
) -> H2Result<TableDef> {
    let table_name = name.to_string();
    let mut col_defs = Vec::new();

    let mut pk_columns: Vec<String> = Vec::new();
    for constraint in constraints {
        if let TableConstraint::PrimaryKey { columns, .. } = constraint {
            for col in columns {
                pk_columns.push(col.value.clone());
            }
        }
    }

    for col in columns {
        let col_name = col.name.value.clone();
        let dt = convert_data_type(&col.data_type)?;
        let mut is_pk = pk_columns.iter().any(|pk| pk.eq_ignore_ascii_case(&col_name));
        let mut is_nullable = true;

        for opt in &col.options {
            match &opt.option {
                sqlparser::ast::ColumnOption::Unique { is_primary, .. } => {
                    if *is_primary {
                        is_pk = true;
                    }
                }
                sqlparser::ast::ColumnOption::NotNull => {
                    is_nullable = false;
                }
                _ => {}
            }
        }

        col_defs.push(ColumnDef {
            name: col_name,
            data_type: dt,
            is_nullable,
            is_primary_key: is_pk,
        });
    }

    Ok(TableDef::new(table_name, col_defs))
}
