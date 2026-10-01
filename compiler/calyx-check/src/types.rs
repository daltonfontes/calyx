//! Types and the assignability relation.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Text,
    Nat,
    Int,
    Float,
    Bool,
    Money,
    Duration,
    Date,
    Unit,
    /// An integer literal without unit: fits `Int`, `Nat` and `Float`.
    IntLit,
    /// `List[T]`, with an optional maximum length.
    List(Box<Ty>, Option<u64>),
    Map(Box<Ty>, Box<Ty>),
    /// A type declared with `type`.
    User(String),
    /// What `try` gives: `Ok(value: T)` or `Failed(error: Text)` (D11).
    Result(Box<Ty>),
    /// Poison: produced after an error, compatible with everything, so one
    /// mistake does not cascade into many diagnostics.
    Error,
}

impl Ty {
    pub fn builtin(name: &str) -> Option<Ty> {
        Some(match name {
            "Text" => Ty::Text,
            "Nat" => Ty::Nat,
            "Int" => Ty::Int,
            "Float" => Ty::Float,
            "Bool" => Ty::Bool,
            "Money" => Ty::Money,
            "Duration" => Ty::Duration,
            "Date" => Ty::Date,
            "Unit" => Ty::Unit,
            _ => return None,
        })
    }

    /// Type of a number literal with the given unit.
    pub fn of_unit(unit: Option<&str>, integer: bool) -> Ty {
        match unit {
            None if integer => Ty::IntLit,
            None => Ty::Float,
            Some("USD" | "BRL" | "EUR") => Ty::Money,
            Some("ms" | "s" | "min" | "h" | "days") => Ty::Duration,
            Some(_) => Ty::Nat, // tokens, sizes, rates
        }
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Text => f.write_str("Text"),
            Ty::Nat => f.write_str("Nat"),
            Ty::Int | Ty::IntLit => f.write_str("Int"),
            Ty::Float => f.write_str("Float"),
            Ty::Bool => f.write_str("Bool"),
            Ty::Money => f.write_str("Money"),
            Ty::Duration => f.write_str("Duration"),
            Ty::Date => f.write_str("Date"),
            Ty::Unit => f.write_str("Unit"),
            Ty::List(t, None) => write!(f, "List[{t}]"),
            Ty::List(t, Some(n)) => write!(f, "List[{t}] max {n}"),
            Ty::Map(k, v) => write!(f, "Map[{k}, {v}]"),
            Ty::User(n) => f.write_str(n),
            Ty::Result(t) => write!(f, "Result[{t}]"),
            Ty::Error => f.write_str("?"),
        }
    }
}

/// Can a value of type `from` be used where `to` is expected?
pub fn assignable(from: &Ty, to: &Ty) -> bool {
    match (from, to) {
        (Ty::Error, _) | (_, Ty::Error) => true,
        (Ty::IntLit, Ty::Int | Ty::Nat | Ty::Float | Ty::IntLit) => true,
        (Ty::Nat, Ty::Int) => true,
        (Ty::List(a, m1), Ty::List(b, m2)) => {
            assignable(a, b)
                && match (m1, m2) {
                    (_, None) => true,
                    (Some(x), Some(y)) => x <= y,
                    (None, Some(_)) => false,
                }
        }
        (Ty::Map(k1, v1), Ty::Map(k2, v2)) => assignable(k1, k2) && assignable(v1, v2),
        (Ty::Result(a), Ty::Result(b)) => assignable(a, b),
        _ => from == to,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(t: Ty, max: Option<u64>) -> Ty {
        Ty::List(Box::new(t), max)
    }

    #[test]
    fn assignability() {
        assert!(assignable(&Ty::IntLit, &Ty::Nat));
        assert!(assignable(&Ty::Nat, &Ty::Int));
        assert!(!assignable(&Ty::Int, &Ty::Nat));
        assert!(!assignable(&Ty::Text, &Ty::Nat));
        assert!(assignable(
            &list(Ty::Text, Some(3)),
            &list(Ty::Text, Some(5))
        ));
        assert!(assignable(&list(Ty::Text, Some(3)), &list(Ty::Text, None)));
        assert!(!assignable(
            &list(Ty::Text, Some(9)),
            &list(Ty::Text, Some(5))
        ));
        assert!(!assignable(&list(Ty::Text, None), &list(Ty::Text, Some(5))));
        assert!(assignable(&Ty::Error, &Ty::Text));
    }

    #[test]
    fn display() {
        assert_eq!(list(Ty::Text, Some(5)).to_string(), "List[Text] max 5");
        assert_eq!(Ty::IntLit.to_string(), "Int");
    }
}
