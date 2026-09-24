pub mod data_type;
pub mod error;
pub mod value;

pub use data_type::DataType;
pub use error::{H2Error, H2Result};
pub use value::Value;

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

    #[test]
    fn test_vector_cosine_similarity() {
        let vec1 = Value::Vector(vec![1.0, 0.0, 0.0]);
        let vec2 = Value::Vector(vec![0.5, 0.5, 0.0]);
        let sim = vec1.cosine_similarity(&vec2).unwrap();
        assert!((sim - 0.7071).abs() < 0.001);
    }
}
