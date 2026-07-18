//! Runtime values. XML attributes are strings; evaluation is dynamically
//! typed — numeric semantics kick in whenever both operands parse as numbers.

use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Num(f64),
    Bool(bool),
    /// A missing attribute. Comparisons against it are always false.
    Null,
}

impl Value {
    pub fn as_num(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            Value::Str(s) => s.trim().parse::<f64>().ok(),
            Value::Bool(_) | Value::Null => None,
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::Null => false,
        }
    }

    /// Equality: numeric when both sides are numbers, string otherwise.
    /// `Null` never equals anything (including `Null`).
    pub fn loose_eq(&self, other: &Value) -> bool {
        if matches!(self, Value::Null) || matches!(other, Value::Null) {
            return false;
        }
        match (self.as_num(), other.as_num()) {
            (Some(a), Some(b)) => a == b,
            _ => self.to_display() == other.to_display(),
        }
    }

    /// Ordering: numeric when both sides are numbers, lexicographic otherwise.
    /// `None` when either side is `Null` (all comparisons become false).
    pub fn compare(&self, other: &Value) -> Option<Ordering> {
        if matches!(self, Value::Null) || matches!(other, Value::Null) {
            return None;
        }
        match (self.as_num(), other.as_num()) {
            (Some(a), Some(b)) => a.partial_cmp(&b),
            _ => Some(self.to_display().cmp(&other.to_display())),
        }
    }

    /// String form used when writing back into XML attributes. Whole numbers
    /// print without a trailing `.0`.
    pub fn to_display(&self) -> String {
        match self {
            Value::Str(s) => s.clone(),
            Value::Num(n) => {
                if n.fract() == 0.0 && n.abs() < 1e15 {
                    format!("{}", *n as i64)
                } else {
                    format!("{n}")
                }
            }
            Value::Bool(b) => b.to_string(),
            Value::Null => String::new(),
        }
    }
}
