//! The typed `Filter` of a query or a search: checked against the columns
//! of the collections in scope, then turned into a DataFusion expression
//! that Lance runs on a table. No text of a query language is ever made.
//!
//! A condition is true or false, never unknown: a leaf on a Document
//! without a value in its column is false, and `none_of` it is true. SQL's
//! three-valued logic would make `NOT` of an unknown leaf unknown, so a
//! negation is pushed down to the leaves, each of which says what "not"
//! means for a missing value (`column IS NULL OR …`). The leaves keep the
//! plain forms Lance reads its scalar indices with.

use lance::deps::datafusion::functions::expr_fn::starts_with;
use lance::deps::datafusion::functions_nested::expr_fn::{array_has, cardinality};
use lance::deps::datafusion::prelude::{Expr, col, lit};
use scheda_api::v1::filter::value::Kind as ValueKind;
use scheda_api::v1::filter::{Bound, Kind as FilterKind, Value};
use scheda_api::v1::{Filter, VaultErrorCode};

use crate::error::Fault;
use crate::store::{Column, Kind};

/// The column every collection has, besides the Projector's.
const ID: &str = "id";

fn invalid(message: impl Into<String>) -> Fault {
    Fault::new(VaultErrorCode::InvalidArgument, message)
}

/// The kind of `name` in a collection of `columns`: `id` is a text;
/// `None` when the collection does not declare it.
fn kind_in(columns: &[Column], name: &str) -> Option<Kind> {
    if name == ID {
        return Some(Kind::Text);
    }
    columns
        .iter()
        .find(|column| column.name == name)
        .map(|column| column.kind)
}

/// How deep `filter` is: a leaf is one level, each list one more.
fn depth(filter: &Filter) -> usize {
    match &filter.kind {
        Some(FilterKind::AllOf(list) | FilterKind::AnyOf(list) | FilterKind::NoneOf(list)) => {
            1 + list.filters.iter().map(depth).max().unwrap_or(0)
        }
        _ => 1,
    }
}

/// Whether `value` is of the kind of a column of `kind` (the element kind
/// of a `text list`).
fn fits(kind: Kind, value: &Value) -> bool {
    matches!(
        (kind, &value.kind),
        (Kind::Text | Kind::TextList, Some(ValueKind::Text(_)))
            | (Kind::Integer, Some(ValueKind::Integer(_)))
            | (Kind::Number, Some(ValueKind::Number(_)))
            | (Kind::Boolean, Some(ValueKind::Boolean(_)))
    )
}

/// Checks `filter` against the collections of `scope`, each given by its
/// columns: at most `max_depth` deep; every leaf names `id` or a column at
/// least one collection declares, and in each collection that declares it a
/// value of its kind and a leaf its kind allows.
///
/// # Errors
///
/// `INVALID_ARGUMENT` naming what breaks a rule.
pub(crate) fn check(filter: &Filter, scope: &[&[Column]], max_depth: u32) -> Result<(), Fault> {
    if depth(filter) > max_depth as usize {
        return Err(invalid(format!(
            "the filter is deeper than {max_depth} levels (OpenOptions.filter_max_depth)"
        )));
    }
    check_node(filter, scope)
}

fn check_node(filter: &Filter, scope: &[&[Column]]) -> Result<(), Fault> {
    match &filter.kind {
        Some(FilterKind::AllOf(list) | FilterKind::AnyOf(list) | FilterKind::NoneOf(list)) => list
            .filters
            .iter()
            .try_for_each(|filter| check_node(filter, scope)),
        Some(FilterKind::Term(term)) => {
            let value = term.value.as_option();
            check_leaf(scope, term.column.as_deref(), "term", |kind| {
                value.is_some_and(|value| fits(kind, value))
            })
        }
        Some(FilterKind::Terms(terms)) => {
            check_leaf(scope, terms.column.as_deref(), "terms", |kind| {
                terms.values.iter().all(|value| fits(kind, value))
            })
        }
        Some(FilterKind::Range(range)) => {
            let bounds = [range.lower.as_option(), range.upper.as_option()];
            check_leaf(scope, range.column.as_deref(), "range", |kind| {
                !matches!(kind, Kind::Boolean | Kind::TextList)
                    && bounds.iter().flatten().all(|bound| {
                        bound
                            .value
                            .as_option()
                            .is_some_and(|value| fits(kind, value))
                    })
            })
        }
        Some(FilterKind::Exists(exists)) => {
            if exists.column.as_deref() == Some(ID) {
                return Err(invalid("exists names a column; every Document has an id"));
            }
            check_leaf(scope, exists.column.as_deref(), "exists", |_| true)
        }
        Some(FilterKind::Prefix(prefix)) => {
            check_leaf(scope, prefix.column.as_deref(), "prefix", |kind| {
                kind == Kind::Text
            })
        }
        None => Err(invalid("a filter names no condition")),
    }
}

/// Checks one leaf on `column`: some collection declares it, and `allows`
/// the kind it has in each one that does.
fn check_leaf(
    scope: &[&[Column]],
    column: Option<&str>,
    leaf: &str,
    allows: impl Fn(Kind) -> bool,
) -> Result<(), Fault> {
    let name = column.unwrap_or_default();
    let mut declared = false;
    for columns in scope {
        let Some(kind) = kind_in(columns, name) else {
            continue;
        };
        declared = true;
        if !allows(kind) {
            return Err(invalid(format!(
                "{leaf} on the column {name}: a value or a condition its kind does not take"
            )));
        }
    }
    if declared {
        Ok(())
    } else {
        Err(invalid(format!(
            "the column {name} is not declared by any collection in scope"
        )))
    }
}

/// The literal of `value`.
fn literal(value: &Value) -> Expr {
    match &value.kind {
        Some(ValueKind::Text(text)) => lit(text.clone()),
        Some(ValueKind::Integer(n)) => lit(*n),
        Some(ValueKind::Number(n)) => lit(*n),
        Some(ValueKind::Boolean(b)) => lit(*b),
        None => lit(false),
    }
}

/// The expression of `filter`, checked already, for a collection of
/// `columns`: a leaf on a column it does not declare is false.
pub(crate) fn compile(filter: &Filter, columns: &[Column]) -> Expr {
    expression(filter, columns, false)
}

/// `filter` (`negated`: its negation), with the negation pushed down to
/// the leaves.
fn expression(filter: &Filter, columns: &[Column], negated: bool) -> Expr {
    match &filter.kind {
        Some(FilterKind::AllOf(list)) => {
            let parts = list.filters.iter().map(|f| expression(f, columns, negated));
            if negated { any(parts) } else { all(parts) }
        }
        Some(FilterKind::AnyOf(list)) => {
            let parts = list.filters.iter().map(|f| expression(f, columns, negated));
            if negated { all(parts) } else { any(parts) }
        }
        Some(FilterKind::NoneOf(list)) => {
            let parts = list
                .filters
                .iter()
                .map(|f| expression(f, columns, !negated));
            if negated { any(parts) } else { all(parts) }
        }
        Some(FilterKind::Term(term)) => {
            let name = term.column.as_deref().unwrap_or_default();
            let value = term.value.as_option();
            leaf(columns, name, negated, |kind, column| match (kind, value) {
                (Kind::TextList, Some(value)) => array_has(column, literal(value)),
                (_, Some(value)) => column.eq(literal(value)),
                (_, None) => lit(false),
            })
        }
        Some(FilterKind::Terms(terms)) => {
            let name = terms.column.as_deref().unwrap_or_default();
            leaf(columns, name, negated, |kind, column| {
                if kind == Kind::TextList {
                    any(terms
                        .values
                        .iter()
                        .map(|value| array_has(column.clone(), literal(value))))
                } else {
                    column.in_list(terms.values.iter().map(literal).collect(), false)
                }
            })
        }
        Some(FilterKind::Range(range)) => {
            let name = range.column.as_deref().unwrap_or_default();
            let lower = range.lower.as_option().cloned();
            let upper = range.upper.as_option().cloned();
            leaf(columns, name, negated, |_, column| {
                let mut parts = Vec::new();
                if let Some(bound) = &lower {
                    parts.push(after(&column, bound));
                }
                if let Some(bound) = &upper {
                    parts.push(before(&column, bound));
                }
                all(parts.into_iter())
            })
        }
        Some(FilterKind::Exists(exists)) => {
            let name = exists.column.as_deref().unwrap_or_default();
            match kind_in(columns, name) {
                None => lit(negated),
                Some(Kind::TextList) => {
                    let column = col(name);
                    if negated {
                        column
                            .clone()
                            .is_null()
                            .or(cardinality(column).eq(lit(0_u64)))
                    } else {
                        cardinality(column).gt(lit(0_u64))
                    }
                }
                Some(_) => {
                    if negated {
                        col(name).is_null()
                    } else {
                        col(name).is_not_null()
                    }
                }
            }
        }
        Some(FilterKind::Prefix(prefix)) => {
            let name = prefix.column.as_deref().unwrap_or_default();
            let text = prefix.prefix.clone().unwrap_or_default();
            leaf(columns, name, negated, |_, column| {
                starts_with(column, lit(text))
            })
        }
        None => lit(negated),
    }
}

/// A leaf on `name`: `make` gives the condition; its negation is true for a
/// Document without a value. A column the collection does not declare has
/// no value anywhere.
fn leaf(
    columns: &[Column],
    name: &str,
    negated: bool,
    make: impl FnOnce(Kind, Expr) -> Expr,
) -> Expr {
    let Some(kind) = kind_in(columns, name) else {
        return lit(negated);
    };
    let condition = make(kind, col(name));
    if negated {
        col(name).is_null().or(!condition)
    } else {
        condition
    }
}

/// `column` past the lower `bound`.
fn after(column: &Expr, bound: &Bound) -> Expr {
    let value = bound.value.as_option().map_or_else(|| lit(false), literal);
    if bound.inclusive.unwrap_or(false) {
        column.clone().gt_eq(value)
    } else {
        column.clone().gt(value)
    }
}

/// `column` short of the upper `bound`.
fn before(column: &Expr, bound: &Bound) -> Expr {
    let value = bound.value.as_option().map_or_else(|| lit(false), literal);
    if bound.inclusive.unwrap_or(false) {
        column.clone().lt_eq(value)
    } else {
        column.clone().lt(value)
    }
}

/// Every one of `parts`; true for none.
pub(crate) fn all(parts: impl Iterator<Item = Expr>) -> Expr {
    parts.reduce(Expr::and).unwrap_or_else(|| lit(true))
}

/// At least one of `parts`; false for none.
pub(crate) fn any(parts: impl Iterator<Item = Expr>) -> Expr {
    parts.reduce(Expr::or).unwrap_or_else(|| lit(false))
}

#[cfg(test)]
mod tests {
    use scheda_api::v1::Filter;

    use super::{check, depth};
    use crate::store::{Column, Kind};

    fn filter(json: serde_json::Value) -> Filter {
        serde_json::from_value(json).unwrap()
    }

    fn column(name: &str, kind: Kind) -> Column {
        Column {
            name: name.to_owned(),
            kind,
            scalar_index: false,
            fts: false,
            vector: false,
        }
    }

    #[test]
    fn a_leaf_is_one_level_and_each_list_one_more() {
        let leaf = serde_json::json!({"term": {"column": "id", "value": {"text": "a"}}});
        assert_eq!(depth(&filter(leaf.clone())), 1);
        let nested = serde_json::json!({"allOf": {"filters": [{"anyOf": {"filters": [leaf]}}]}});
        assert_eq!(depth(&filter(nested)), 3);
    }

    #[test]
    fn a_column_no_collection_declares_and_a_value_of_another_kind_are_invalid() {
        let notes = [column("n", Kind::Integer), column("tags", Kind::TextList)];
        let scope: [&[Column]; 1] = [&notes];
        let ok = |json| check(&filter(json), &scope, 32);
        assert!(ok(serde_json::json!({"term": {"column": "n", "value": {"integer": 3}}})).is_ok());
        assert!(
            ok(serde_json::json!({"term": {"column": "tags", "value": {"text": "x"}}})).is_ok()
        );
        assert!(ok(serde_json::json!({"term": {"column": "n", "value": {"text": "3"}}})).is_err());
        assert!(
            ok(serde_json::json!({"term": {"column": "zzz", "value": {"text": "3"}}})).is_err()
        );
        assert!(
            ok(serde_json::json!({"range": {"column": "tags", "lower": {"value": {"text": "a"}}}}))
                .is_err()
        );
        assert!(ok(serde_json::json!({"prefix": {"column": "n", "prefix": "1"}})).is_err());
        assert!(ok(serde_json::json!({"exists": {"column": "id"}})).is_err());
        let deep = serde_json::json!({"noneOf": {"filters": [{"exists": {"column": "n"}}]}});
        assert!(check(&filter(deep), &scope, 1).is_err());
    }
}
