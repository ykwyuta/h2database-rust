pub mod data_type;
pub mod error;
pub mod timeout;
pub mod value;

pub use data_type::DataType;
pub use error::{H2Error, H2Result};
pub use timeout::{check_query_timeout, remaining_query_timeout, set_query_timeout, TimeoutGuard};
pub use value::{FromSql, Value, IntervalValue};


#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_value_order_and_types() {
        let v1 = Value::Integer(10);
        let v2 = Value::Integer(20);
        assert!(v1 < v2);

        let d1 = Value::Decimal(rust_decimal::Decimal::new(10050, 2));
        let d2 = Value::Decimal(rust_decimal::Decimal::new(10075, 2));
        assert!(d1 < d2);
    }
}
