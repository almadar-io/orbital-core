//! Display and the conversions into an `SExpression`.

use super::*;

impl fmt::Display for SExpression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SExpression::Null => write!(f, "null"),
            SExpression::Bool(b) => write!(f, "{}", b),
            SExpression::Number(n) => write!(f, "{}", n),
            SExpression::String(s) => write!(f, "\"{}\"", s),
            SExpression::Array(arr) => {
                write!(f, "[")?;
                for (i, item) in arr.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", item)?;
                }
                write!(f, "]")
            }
            SExpression::Object(obj) => {
                write!(f, "{{")?;
                for (i, (k, v)) in obj.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "\"{}\": {}", k, v)?;
                }
                write!(f, "}}")
            }
        }
    }
}

impl From<bool> for SExpression {
    fn from(b: bool) -> Self {
        SExpression::Bool(b)
    }
}

impl From<i32> for SExpression {
    fn from(n: i32) -> Self {
        SExpression::Number(n as f64)
    }
}

impl From<i64> for SExpression {
    fn from(n: i64) -> Self {
        SExpression::Number(n as f64)
    }
}

impl From<f64> for SExpression {
    fn from(n: f64) -> Self {
        SExpression::Number(n)
    }
}

impl From<&str> for SExpression {
    fn from(s: &str) -> Self {
        SExpression::string(s.to_string())
    }
}

impl From<String> for SExpression {
    fn from(s: String) -> Self {
        SExpression::string(s)
    }
}

impl<T: Into<SExpression>> From<Vec<T>> for SExpression {
    fn from(v: Vec<T>) -> Self {
        SExpression::Array(v.into_iter().map(Into::into).collect())
    }
}
