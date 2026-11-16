//! SQL text -> engine [`Query`]. Only module that touches `sqlparser`

pub mod errors;

use sqlparser::ast::{
    BinaryOperator, Expr, LimitClause, SelectItem, SetExpr, Statement, TableFactor, Value,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use crate::query::errors::QueryError;

const DIALECT: GenericDialect = GenericDialect {};

/// Comparison operators
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
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
pub enum Literal {
    /// Numeric text, e.g. `"37500"`, `"-1.5"`.
    Number(String),
    /// Single-quoted string contents.
    String(String),
    /// `TRUE` / `FALSE`.
    Boolean(bool),
}

/// Columns a query returns
#[derive(Debug)]
pub enum Projection {
    /// `SELECT *`
    All,
    /// `SELECT a, b, ...`, in order
    Columns(Vec<String>),
}

/// `column op literal`. Filters in a [`Query`] are AND-ed together
#[derive(Debug)]
pub struct Filter {
    /// Column name, not resolved yet
    pub column: String,
    /// Comparison
    pub op: CompareOp,
    /// Value to compare against
    pub value: Literal,
}

/// Single-table `SELECT` with names not resolved yet
#[derive(Debug)]
pub struct Query {
    /// Table in `FROM`
    pub table: String,
    /// `SELECT` list
    pub projection: Projection,
    /// `WHERE` filters
    pub filters: Vec<Filter>,
    /// `LIMIT n`
    pub limit: Option<u64>,
}

/// Parses one SQL statement into a [`Query`].
///
/// # Errors
///
/// Invalid SQL, or SQL outside the supported subset.
pub fn parse(sql: &str) -> Result<Query, QueryError> {
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
    let Expr::Value(value) = expr else {
        return None;
    };
    match &value.value {
        Value::Number(n, _) => Some(Literal::Number(n.clone())),
        Value::SingleQuotedString(s) => Some(Literal::String(s.clone())),
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
