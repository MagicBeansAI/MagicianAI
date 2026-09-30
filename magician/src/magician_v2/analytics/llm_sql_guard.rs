//! Shared structural guard for caller-supplied LLM analytics SQL.

use std::ops::ControlFlow;

use sqlparser::{
    ast::{Query, SetExpr, Statement, Visit, Visitor},
    dialect::DuckDbDialect,
    parser::{Parser, ParserError},
};

/// Nesting depth allowed in caller-supplied analytics SQL.
///
/// sqlparser is recursive descent over an AST whose frames are large, and its
/// own default (`DEFAULT_REMAINING_DEPTH`, 50) does not fit an actix worker's
/// 2 MiB stack: a deeply nested query overflows and aborts the entire process
/// before the limit is ever reached. That turns any caller who can submit SQL
/// into a denial of service on the whole server, which no parse result is worth.
///
/// 20 is far below what the stack can absorb and far above anything a real
/// analytics query needs; exceeding it is a rejected request, not a crash.
pub const ANALYTICS_SQL_RECURSION_LIMIT: usize = 20;

/// Parse caller-supplied analytics SQL with a stack-safe recursion bound.
///
/// Every analytics parse must go through this rather than
/// `Parser::parse_sql`, which silently takes the unsafe default.
pub fn parse_analytics_sql(sql: &str) -> Result<Vec<Statement>, ParserError> {
    Parser::new(&DuckDbDialect {})
        .with_recursion_limit(ANALYTICS_SQL_RECURSION_LIMIT)
        .try_with_sql(sql)?
        .parse_statements()
}

/// Parse exactly one statement and require the guarded read-only query shape.
pub fn is_one_read_only_select_statement(sql: &str) -> bool {
    matches!(
        parse_analytics_sql(sql).as_deref(),
        Ok([Statement::Query(query)]) if is_read_only_select_query(query)
    )
}

/// Return true only for side-effect-free read-only query bodies.
///
/// `Statement::Query` alone is not a sufficient read-only boundary in
/// sqlparser: a query body can contain `TABLE`, mutation-shaped set
/// expressions, or a `SELECT INTO`. Nested query nodes are checked by each SQL
/// visitor as they are entered; `SetExpr::Query` is also handled here so a
/// parenthesized set operand cannot bypass the guard.
///
/// `VALUES` is deliberately rejected even though it is side-effect-free. These
/// guarded surfaces promise a SELECT/WITH-only query contract, and allowing a
/// second query-body shape would let literal CTEs and derived tables bypass the
/// same structural boundary enforced for relation-backed SELECTs.
pub fn is_read_only_select_query(query: &Query) -> bool {
    let mut visitor = ReadOnlyQueryVisitor;
    matches!(query.visit(&mut visitor), ControlFlow::Continue(()))
}

struct ReadOnlyQueryVisitor;

impl Visitor for ReadOnlyQueryVisitor {
    type Break = ();

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        if is_read_only_select_query_node(query) {
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break(())
        }
    }
}

/// Check one query node. The visitor above applies this to CTEs and every
/// expression/derived-table subquery as well as the outer query.
pub fn is_read_only_select_query_node(query: &Query) -> bool {
    query.locks.is_empty()
        && query.settings.is_none()
        && query.format_clause.is_none()
        && query.for_clause.is_none()
        && query.pipe_operators.is_empty()
        && is_read_only_select_body(&query.body)
}

fn is_read_only_select_body(body: &SetExpr) -> bool {
    match body {
        SetExpr::Select(select) => select.into.is_none(),
        SetExpr::Query(query) => is_read_only_select_query_node(query),
        SetExpr::SetOperation { left, right, .. } => {
            is_read_only_select_body(left) && is_read_only_select_body(right)
        },
        SetExpr::Values(_)
        | SetExpr::Insert(_)
        | SetExpr::Update(_)
        | SetExpr::Delete(_)
        | SetExpr::Merge(_)
        | SetExpr::Table(_) => false,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {

    /// A deeply nested query previously aborted the whole process: sqlparser is
    /// recursive descent with large AST frames, and its own default depth of 50
    /// does not fit an actix worker's 2 MiB stack — the overflow fired before
    /// the limit was ever reached. Any caller able to submit SQL could take the
    /// server down.
    #[test]
    fn deeply_nested_sql_is_rejected_rather_than_overflowing_the_stack() {
        let nested = format!(
            "SELECT {}1{}",
            "(".repeat(ANALYTICS_SQL_RECURSION_LIMIT * 40),
            ")".repeat(ANALYTICS_SQL_RECURSION_LIMIT * 40)
        );
        assert!(parse_analytics_sql(&nested).is_err());
        assert!(!is_one_read_only_select_statement(&nested));
    }

    /// The bound must not reject the nesting real analytics queries use.
    #[test]
    fn ordinary_nesting_still_parses() {
        let sql = "SELECT a FROM t WHERE id IN (SELECT id FROM u WHERE x = (1 + (2 * 3)))";
        assert!(parse_analytics_sql(sql).is_ok());
        assert!(is_one_read_only_select_statement(sql));
    }
    use super::*;

    fn parsed_query(sql: &str) -> Box<Query> {
        let mut statements = Parser::parse_sql(&DuckDbDialect {}, sql).expect("parse query");
        let sqlparser::ast::Statement::Query(query) = statements.pop().expect("one statement")
        else {
            panic!("expected query statement")
        };
        query
    }

    #[test]
    fn permits_selects_and_set_operations() {
        for sql in [
            "SELECT * FROM llm_calls",
            "SELECT * FROM llm_calls UNION ALL SELECT * FROM llm_calls",
            "WITH recent AS (SELECT * FROM llm_calls) SELECT * FROM recent",
        ] {
            assert!(is_read_only_select_query(&parsed_query(sql)), "{sql}");
        }
    }

    #[test]
    fn rejects_non_select_query_bodies_and_select_into() {
        for sql in [
            "SELECT * INTO temporary_llm_copy FROM llm_calls",
            "VALUES (1)",
            "WITH payload AS (VALUES (1)) SELECT * FROM payload",
            "SELECT * FROM (VALUES (1)) AS payload(value)",
            "TABLE llm_calls",
        ] {
            assert!(!is_one_read_only_select_statement(sql), "{sql}");
        }
        assert!(!is_one_read_only_select_statement(
            "SELECT * FROM llm_calls; SELECT * FROM llm_calls"
        ));
    }
}
