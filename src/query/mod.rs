//! SQL text -> engine [`Query`]. Only module that touches `sqlparser`

pub(crate) mod errors;

use sqlparser::ast::{
    BinaryOperator, DataType as SqlType, Expr, LimitClause, SelectItem, SetExpr, Statement,
    TableFactor, UnaryOperator, Value,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use crate::query::errors::QueryError;

const DIALECT: GenericDialect = GenericDialect {};

/// Comparison operators
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompareOp {
    /// `=`
    Eq,
    /// `<>` / `!=`
    NotEq,
    /// `<`
    Lt,
    /// `<=`
    LtEq,
    /// `>`
    Gt,
    /// `>=`
    GtEq,
}

/// Literal as written in the SQL. Left unparsed since only bind knows the column type
#[derive(Debug)]
pub(crate) enum Literal {
    /// Numeric text, e.g. `"37500"`, `"-0.05"`. Compared with `Int32`, `Int64`,
    /// and `Decimal` columns
    Number(String),
    /// Single-quoted string contents. Compared with `Utf8` columns
    String(String),
    /// `DATE '1995-01-01'`, text inside the quotes. Compared with `Date32` columns
    Date(String),
}

/// Columns a query returns
#[derive(Debug)]
pub(crate) enum Projection {
    /// `SELECT *`
    All,
    /// `SELECT a, b, ...`, in order
    Columns(Vec<String>),
}

/// `column op literal`. Filters in a [`Query`] are AND-ed together
#[derive(Debug)]
pub(crate) struct Filter {
    /// Column name, not resolved yet
    pub(crate) column: String,
    /// Comparison
    pub(crate) op: CompareOp,
    /// Value to compare against
    pub(crate) value: Literal,
}

/// Single-table `SELECT` with names not resolved yet
#[derive(Debug)]
pub(crate) struct Query {
    /// Table in `FROM`
    pub(crate) table: String,
    /// `SELECT` list
    pub(crate) projection: Projection,
    /// `WHERE` filters
    pub(crate) filters: Vec<Filter>,
    /// `LIMIT n`
    pub(crate) limit: Option<u64>,
}

/// Parses one SQL statement into a [`Query`].
///
/// # Errors
///
/// Invalid SQL, or SQL outside the supported subset.
pub(crate) fn parse(sql: &str) -> Result<Query, QueryError> {
    let statements = Parser::parse_sql(&DIALECT, sql)?;
    statements
        .first()
        .and_then(query)
        .ok_or_else(|| QueryError::Unsupported(sql.to_string()))
}

fn query(statement: &Statement) -> Option<Query> {
    let Statement::Query(query) = statement else {
        return None;
    };
    let SetExpr::Select(select) = query.body.as_ref() else {
        return None;
    };
    let TableFactor::Table { name, .. } = &select.from.first()?.relation else {
        return None;
    };

    Some(Query {
        table: name.to_string(),
        projection: projection(&select.projection)?,
        filters: match &select.selection {
            Some(expr) => filters(expr)?,
            None => Vec::new(),
        },
        limit: limit(query.limit_clause.as_ref()),
    })
}

fn projection(items: &[SelectItem]) -> Option<Projection> {
    if let [SelectItem::Wildcard(_)] = items {
        return Some(Projection::All);
    }
    items
        .iter()
        .map(|item| match item {
            SelectItem::UnnamedExpr(expr) => column(expr),
            _ => None,
        })
        .collect::<Option<_>>()
        .map(Projection::Columns)
}

/// `a AND b AND ...`, where each leaf is `column op literal`.
fn filters(expr: &Expr) -> Option<Vec<Filter>> {
    match expr {
        Expr::BinaryOp {
            left,
            op: BinaryOperator::And,
            right,
        } => {
            let mut out = filters(left)?;
            out.extend(filters(right)?);
            Some(out)
        }
        Expr::Nested(inner) => filters(inner),
        Expr::BinaryOp { left, op, right } => Some(vec![Filter {
            column: column(left)?,
            op: compare_op(op)?,
            value: literal(right)?,
        }]),
        _ => None,
    }
}

fn column(expr: &Expr) -> Option<String> {
    let Expr::Identifier(ident) = expr else {
        return None;
    };
    Some(ident.value.clone())
}

fn literal(expr: &Expr) -> Option<Literal> {
    match expr {
        Expr::Value(value) => match &value.value {
            Value::Number(n, _) => Some(Literal::Number(n.clone())),
            Value::SingleQuotedString(s) => Some(Literal::String(s.clone())),
            _ => None,
        },
        // `-5` parses as unary minus applied to `5`
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } => match literal(expr)? {
            Literal::Number(n) => Some(Literal::Number(format!("-{n}"))),
            _ => None,
        },
        Expr::TypedString(typed) if typed.data_type == SqlType::Date => match &typed.value.value {
            Value::SingleQuotedString(s) => Some(Literal::Date(s.clone())),
            _ => None,
        },
        _ => None,
    }
}

fn compare_op(op: &BinaryOperator) -> Option<CompareOp> {
    match op {
        BinaryOperator::Eq => Some(CompareOp::Eq),
        BinaryOperator::NotEq => Some(CompareOp::NotEq),
        BinaryOperator::Lt => Some(CompareOp::Lt),
        BinaryOperator::LtEq => Some(CompareOp::LtEq),
        BinaryOperator::Gt => Some(CompareOp::Gt),
        BinaryOperator::GtEq => Some(CompareOp::GtEq),
        _ => None,
    }
}

fn limit(clause: Option<&LimitClause>) -> Option<u64> {
    let LimitClause::LimitOffset {
        limit: Some(expr), ..
    } = clause?
    else {
        return None;
    };
    let Literal::Number(n) = literal(expr)? else {
        return None;
    };
    n.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::{Literal, parse};

    fn first_literal(sql: &str) -> Option<Literal> {
        parse(sql)
            .ok()
            .and_then(|q| q.filters.into_iter().next())
            .map(|f| f.value)
    }

    #[test]
    fn literal_per_type() {
        let number = first_literal("SELECT * FROM t WHERE a <= 37500");
        assert!(matches!(number, Some(Literal::Number(n)) if n == "37500"));

        let negative = first_literal("SELECT * FROM t WHERE a > -0.05");
        assert!(matches!(negative, Some(Literal::Number(n)) if n == "-0.05"));

        let string = first_literal("SELECT * FROM t WHERE a = 'x'");
        assert!(matches!(string, Some(Literal::String(s)) if s == "x"));

        let date = first_literal("SELECT * FROM t WHERE a < DATE '1995-03-15'");
        assert!(matches!(date, Some(Literal::Date(d)) if d == "1995-03-15"));
    }
}
