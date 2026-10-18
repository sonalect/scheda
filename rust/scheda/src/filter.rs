//! Compiles a `Filter` into a Tantivy query over the lexical index.
//!
//! A Filter only selects: every kind is a condition that holds or does not,
//! so the query it compiles to is never asked for a score. The leaves name a
//! column the Projector declared `find` for a collection, or `id`.
//!
//! **Scope.** In a query of one collection a leaf on a Projector column
//! means that collection (a `collection` set on it must name it). In a
//! query of the Vault every such leaf names its collection. A Filter is
//! compiled once for each collection that is scanned: a leaf that names
//! another collection is false there, so `a.title = x OR b.title = y`
//! selects the Documents of `a` with that title and those of `b` with that
//! one. Every leaf is checked against the columns of the collection it
//! names, scanned or not.
//!
//! **Types.** The columns come from the `columns` file the Index was built
//! with, so a leaf is checked against what was indexed, not against what
//! the Projector says now. A value of another type than the column
//! (a number for a text column, a fraction for an integer column) is
//! `INVALID_ARGUMENT`, as is a phrase, a fuzzy term, or a pattern on a
//! column that holds no tokens with positions (the numbers and the
//! booleans).

use std::ops::Bound;

use buffa_types::google::protobuf::Value;
use buffa_types::google::protobuf::value::Kind as ValueKind;
use scheda_api::v1::filter::bound::Kind as BoundKind;
use scheda_api::v1::filter::{self, Kind, Occur};
use scheda_api::v1::{Filter, VaultErrorCode};
use tantivy::Term;
use tantivy::query::{
    AllQuery, BooleanQuery, EmptyQuery, FuzzyTermQuery, Occur as TantivyOccur, PhrasePrefixQuery,
    PhraseQuery, Query, RangeQuery, RegexPhraseQuery, RegexQuery, TermQuery, TermSetQuery,
};
use tantivy::schema::{Field, IndexRecordOption};
use tantivy::tokenizer::TokenStream;

use crate::collection::Declarations;
use crate::error::{Error, Fault, OrFault};
use crate::lexical::{DumpColumn, Snapshot, ValueKind as ColumnKind, eq_term, f64_key, i64_key};

/// How many terms a prefix of a `phrase_prefix` may expand to when the
/// request does not say.
const PREFIX_EXPANSIONS: u32 = 50;

/// How many terms the patterns of a `regex_phrase` may expand to, together,
/// when the request does not say.
const REGEX_EXPANSIONS: u32 = 16_384;

/// Whose columns a leaf of a Filter without a `collection` means.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Scope<'a> {
    /// A query of one collection: the leaf is on that collection.
    Collection(&'a str),
    /// A query of the Vault: every leaf on a Projector column names its
    /// collection.
    Vault,
}

/// A compiler of Filters for one scan: the collection being scanned, and
/// what the checks of the leaves need to know.
pub(crate) struct Compiler<'a> {
    snapshot: &'a Snapshot,
    declarations: &'a Declarations,
    scope: Scope<'a>,
    /// The collection being scanned; `None` compiles to check only.
    target: Option<&'a str>,
}

/// What a leaf names.
enum Leaf<'a> {
    /// The identity of a Document.
    Id,
    /// A column of a collection.
    Column(&'a DumpColumn),
}

/// The tokens a kind of leaf needs.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Needs {
    /// A value of the column, or `id`.
    Value,
    /// Tokens with positions: not `id`, and a text column.
    Tokens,
}

fn invalid(message: impl Into<String>) -> Fault {
    Fault::new(VaultErrorCode::InvalidArgument, message)
}

impl<'a> Compiler<'a> {
    /// A compiler of Filters for the scan of `target` in the scope `scope`,
    /// over the columns `snapshot` holds and the collections
    /// `declarations` declares. Without a target the Filter is only checked.
    pub(crate) fn new(
        snapshot: &'a Snapshot,
        declarations: &'a Declarations,
        scope: Scope<'a>,
        target: Option<&'a str>,
    ) -> Compiler<'a> {
        Compiler {
            snapshot,
            declarations,
            scope,
            target,
        }
    }

    /// The query `filter` stands for on the scanned collection.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` for a leaf without the `collection` the scope
    /// asks for, with another one, on a column the Projector did not
    /// declare `find`, with a value of another type than the column, or
    /// with a pattern that does not compile; `NOT_FOUND` for a `collection`
    /// that is not declared.
    pub(crate) fn compile(&self, filter: &Filter) -> Result<Box<dyn Query>, Error> {
        let Some(kind) = filter.kind.as_ref() else {
            return Err(invalid("the filter names no kind").into());
        };
        Ok(match kind {
            Kind::All(_) => Box::new(AllQuery),
            Kind::Empty(_) => Box::new(EmptyQuery),
            Kind::Term(term) => self.term(term)?,
            Kind::TermSet(set) => self.term_set(set)?,
            Kind::Phrase(phrase) => self.phrase(phrase)?,
            Kind::PhrasePrefix(prefix) => self.phrase_prefix(prefix)?,
            Kind::Fuzzy(fuzzy) => self.fuzzy(fuzzy)?,
            Kind::Regex(regex) => self.regex(regex)?,
            Kind::RegexPhrase(phrase) => self.regex_phrase(phrase)?,
            Kind::Range(range) => self.range(range)?,
            Kind::Exists(exists) => self.exists(exists)?,
            Kind::Boolean(boolean) => self.boolean(boolean)?,
        })
    }

    fn fields(&self) -> crate::lexical::Fields {
        self.snapshot.fields()
    }

    /// What the leaf `field` of `collection` names, and whether it applies
    /// to the scanned collection.
    fn leaf(
        &self,
        field: &str,
        collection: Option<&str>,
        needs: Needs,
    ) -> Result<(Leaf<'a>, bool), Fault> {
        if field == "id" {
            if needs == Needs::Tokens {
                return Err(invalid("this kind of filter cannot use the field id"));
            }
            return Ok((Leaf::Id, true));
        }
        let named: &str = match (self.scope, collection) {
            (Scope::Collection(own), None) => own,
            (Scope::Collection(own), Some(named)) if named == own => own,
            (Scope::Collection(own), Some(named)) => {
                return Err(invalid(format!(
                    "the filter names the collection {named} in a query of {own}"
                )));
            }
            (Scope::Vault, Some(named)) => named,
            (Scope::Vault, None) => {
                return Err(invalid(format!(
                    "a query of the Vault names the collection of the field {field}"
                )));
            }
        };
        let Some(declared) = self.declarations.get(named) else {
            return Err(Fault::new(
                VaultErrorCode::NotFound,
                format!("the collection {named} is not declared"),
            ));
        };
        let columns = self.snapshot.columns(declared.name()).unwrap_or_default();
        let Some(column) = columns
            .iter()
            .find(|column| column.find && column.name == field)
        else {
            return Err(invalid(format!(
                "the field {field} is not a find column of the collection {named}"
            )));
        };
        if needs == Needs::Tokens && !matches!(column.kind, ColumnKind::Text | ColumnKind::TextList)
        {
            return Err(invalid(format!(
                "this kind of filter needs tokens with positions, which the field {field} of {named} does not keep"
            )));
        }
        let applies = self.target == Some(declared.name());
        Ok((Leaf::Column(column), applies))
    }

    /// `query` where the leaf applies to the scanned collection, else the
    /// query that matches nothing.
    fn only_if(applies: bool, query: Box<dyn Query>) -> Box<dyn Query> {
        if applies { query } else { Box::new(EmptyQuery) }
    }

    fn term(&self, term: &filter::Term) -> Result<Box<dyn Query>, Error> {
        let field = term.field.as_deref().unwrap_or_default();
        let (leaf, applies) = self.leaf(field, term.collection.as_deref(), Needs::Value)?;
        let value = term
            .value
            .as_option()
            .ok_or_else(|| invalid("the term has no value"))?;
        let query = self.term_query(&leaf, field, value)?;
        Ok(Self::only_if(applies, query))
    }

    fn term_set(&self, set: &filter::TermSet) -> Result<Box<dyn Query>, Error> {
        let field = set.field.as_deref().unwrap_or_default();
        let (leaf, applies) = self.leaf(field, set.collection.as_deref(), Needs::Value)?;
        let values = set
            .values
            .as_option()
            .map(|list| list.values.as_slice())
            .unwrap_or_default();
        // The proto says so (`filter.term_set.values.not_empty`), but
        // protovalidate-buffa does not evaluate a rule on a
        // `google.protobuf.ListValue` field, so the library checks it.
        if values.is_empty() {
            return Err(invalid("a term set holds at least one term").into());
        }
        let mut terms = Vec::with_capacity(values.len());
        for value in values {
            terms.push(self.term_of(&leaf, field, value)?);
        }
        Ok(Self::only_if(applies, Box::new(TermSetQuery::new(terms))))
    }

    fn term_query(
        &self,
        leaf: &Leaf<'_>,
        field: &str,
        value: &Value,
    ) -> Result<Box<dyn Query>, Error> {
        Ok(Box::new(TermQuery::new(
            self.term_of(leaf, field, value)?,
            IndexRecordOption::Basic,
        )))
    }

    /// The Tantivy term of `value` on the leaf: the `id` field for the
    /// identity, the `null` field for a stored null, else the `eq` term of
    /// the column.
    fn term_of(&self, leaf: &Leaf<'_>, field: &str, value: &Value) -> Result<Term, Fault> {
        let fields = self.fields();
        match leaf {
            Leaf::Id => match value.kind.as_ref() {
                Some(ValueKind::StringValue(id)) => Ok(Term::from_field_text(fields.id, id)),
                _ => Err(invalid("a term on id is a string")),
            },
            Leaf::Column(column) => match column_text(column, value)? {
                None => Ok(Term::from_field_text(fields.null, field)),
                Some(text) => Ok(Term::from_field_text(fields.eq, &eq(field, &text)?)),
            },
        }
    }

    fn exists(&self, exists: &filter::Exists) -> Result<Box<dyn Query>, Error> {
        let field = exists.field.as_deref().unwrap_or_default();
        let (leaf, applies) = self.leaf(field, exists.collection.as_deref(), Needs::Value)?;
        // A column is flat: `json_subpaths` has no paths below it to count.
        let query: Box<dyn Query> = match leaf {
            Leaf::Id => Box::new(AllQuery),
            Leaf::Column(_) => Box::new(TermQuery::new(
                Term::from_field_text(self.fields().exists, field),
                IndexRecordOption::Basic,
            )),
        };
        Ok(Self::only_if(applies, query))
    }

    /// The tokens the analyzer of the `tok` field makes of `text`.
    fn tokens(&self, text: &str) -> Result<Vec<String>, Fault> {
        let mut analyzer = self
            .snapshot
            .tantivy()
            .tokenizer_for_field(self.fields().tok)
            .or_fault(
                VaultErrorCode::IndexBroken,
                "open the tokenizer of the lexical index",
            )?;
        let mut stream = analyzer.token_stream(text);
        let mut tokens = Vec::new();
        while stream.advance() {
            tokens.push(stream.token().text.clone());
        }
        Ok(tokens)
    }

    /// The `tok` terms of `column` for `tokens`.
    fn token_terms(&self, column: &str, tokens: &[String]) -> Result<Vec<Term>, Fault> {
        tokens
            .iter()
            .map(|token| {
                eq(column, token).map(|text| Term::from_field_text(self.fields().tok, &text))
            })
            .collect()
    }

    fn phrase(&self, phrase: &filter::Phrase) -> Result<Box<dyn Query>, Error> {
        let field = phrase.field.as_deref().unwrap_or_default();
        let (_, applies) = self.leaf(field, phrase.collection.as_deref(), Needs::Tokens)?;
        let mut tokens = Vec::new();
        for text in &phrase.terms {
            tokens.extend(self.tokens(text)?);
        }
        if tokens.len() < 2 {
            return Err(invalid("a phrase holds at least two tokens").into());
        }
        let mut query = PhraseQuery::new(self.token_terms(field, &tokens)?);
        if let Some(slop) = phrase.slop {
            query.set_slop(slop);
        }
        Ok(Self::only_if(applies, Box::new(query)))
    }

    fn phrase_prefix(&self, prefix: &filter::PhrasePrefix) -> Result<Box<dyn Query>, Error> {
        let field = prefix.field.as_deref().unwrap_or_default();
        let (_, applies) = self.leaf(field, prefix.collection.as_deref(), Needs::Tokens)?;
        let mut tokens = Vec::new();
        for text in &prefix.terms {
            tokens.extend(self.tokens(text)?);
        }
        let last = prefix.prefix.as_deref().unwrap_or_default();
        let mut ends = self.tokens(last)?;
        if ends.is_empty() {
            ends.push(last.to_lowercase());
        }
        tokens.extend(ends);
        if tokens.len() < 2 {
            return Err(invalid("a phrase prefix holds a whole token before the prefix").into());
        }
        let mut query = PhrasePrefixQuery::new(self.token_terms(field, &tokens)?);
        query.set_max_expansions(
            prefix
                .max_expansions
                .filter(|n| *n > 0)
                .unwrap_or(PREFIX_EXPANSIONS),
        );
        Ok(Self::only_if(applies, Box::new(query)))
    }

    fn fuzzy(&self, fuzzy: &filter::FuzzyTerm) -> Result<Box<dyn Query>, Error> {
        let field = fuzzy.field.as_deref().unwrap_or_default();
        let (_, applies) = self.leaf(field, fuzzy.collection.as_deref(), Needs::Tokens)?;
        let text = fuzzy.term.as_deref().unwrap_or_default();
        let tokens = self.tokens(text)?;
        let token = match tokens.as_slice() {
            [one] => one.as_str(),
            [] => text,
            _ => return Err(invalid("a fuzzy term is one token").into()),
        };
        let term = Term::from_field_text(self.fields().tok, &eq(field, token)?);
        // The proto keeps the distance within 0 to 2.
        let distance = u8::try_from(fuzzy.distance.unwrap_or(0)).unwrap_or(u8::MAX);
        let transposition = fuzzy.transposition_cost_one.unwrap_or(false);
        let query = if fuzzy.prefix.unwrap_or(false) {
            FuzzyTermQuery::new_prefix(term, distance, transposition)
        } else {
            FuzzyTermQuery::new(term, distance, transposition)
        };
        Ok(Self::only_if(applies, Box::new(query)))
    }

    fn regex(&self, regex: &filter::Regex) -> Result<Box<dyn Query>, Error> {
        let field = regex.field.as_deref().unwrap_or_default();
        let (_, applies) = self.leaf(field, regex.collection.as_deref(), Needs::Tokens)?;
        let pattern = field_pattern(field, regex.pattern.as_deref().unwrap_or_default());
        let query = RegexQuery::from_pattern(&pattern, self.fields().tok).or_fault(
            VaultErrorCode::InvalidArgument,
            "the regex pattern does not compile",
        )?;
        Ok(Self::only_if(applies, Box::new(query)))
    }

    fn regex_phrase(&self, phrase: &filter::RegexPhrase) -> Result<Box<dyn Query>, Error> {
        let field = phrase.field.as_deref().unwrap_or_default();
        let (_, applies) = self.leaf(field, phrase.collection.as_deref(), Needs::Tokens)?;
        let patterns: Vec<String> = phrase
            .patterns
            .iter()
            .map(|pattern| field_pattern(field, pattern))
            .collect();
        // Tantivy compiles the patterns when it weights the query and
        // reports a broken one as a failure of the search; the single regex
        // query uses the same engine, so a pattern it refuses is the
        // caller's mistake and is found here.
        for pattern in &patterns {
            RegexQuery::from_pattern(pattern, self.fields().tok).or_fault(
                VaultErrorCode::InvalidArgument,
                "a pattern of the regex phrase does not compile",
            )?;
        }
        let mut query = RegexPhraseQuery::new(self.fields().tok, patterns);
        if let Some(slop) = phrase.slop {
            query.set_slop(slop);
        }
        query.set_max_expansions(
            phrase
                .max_expansions
                .filter(|n| *n > 0)
                .unwrap_or(REGEX_EXPANSIONS),
        );
        Ok(Self::only_if(applies, Box::new(query)))
    }

    fn range(&self, range: &filter::Range) -> Result<Box<dyn Query>, Error> {
        let field = range.field.as_deref().unwrap_or_default();
        let (leaf, applies) = self.leaf(field, range.collection.as_deref(), Needs::Value)?;
        let lower = range.lower.as_option();
        let upper = range.upper.as_option();
        let query: Box<dyn Query> = match leaf {
            Leaf::Id => {
                let lower = text_bound(self.fields().id, None, lower, false)?;
                let upper = text_bound(self.fields().id, None, upper, true)?;
                if matches!((&lower, &upper), (Bound::Unbounded, Bound::Unbounded)) {
                    Box::new(AllQuery)
                } else {
                    Box::new(RangeQuery::new(lower, upper))
                }
            }
            Leaf::Column(column) => match column.kind {
                ColumnKind::Integer => self.integer_range(field, lower, upper)?,
                ColumnKind::Number => self.number_range(field, lower, upper)?,
                ColumnKind::Text | ColumnKind::TextList => {
                    let eq = self.fields().eq;
                    Box::new(RangeQuery::new(
                        text_bound(eq, Some(field), lower, false)?,
                        text_bound(eq, Some(field), upper, true)?,
                    ))
                }
                ColumnKind::Boolean => {
                    return Err(invalid(format!(
                        "the field {field} is a boolean column, which has no range"
                    ))
                    .into());
                }
            },
        };
        Ok(Self::only_if(applies, query))
    }

    /// A range on a `number` column: the ends compare as the `f64` keys of
    /// the `num` field, whose byte order is the order of the numbers.
    fn number_range(
        &self,
        field: &str,
        lower: Option<&filter::Bound>,
        upper: Option<&filter::Bound>,
    ) -> Result<Box<dyn Query>, Error> {
        let term = |value: f64| -> Result<Term, Fault> {
            Ok(Term::from_field_text(
                self.fields().num,
                &eq(field, &f64_key(value))?,
            ))
        };
        let lower = match numeric_end(field, lower)? {
            None => Bound::Included(term(f64::NEG_INFINITY)?),
            Some((value, true)) => Bound::Excluded(term(value)?),
            Some((value, false)) => Bound::Included(term(value)?),
        };
        let upper = match numeric_end(field, upper)? {
            None => Bound::Included(term(f64::INFINITY)?),
            Some((value, true)) => Bound::Excluded(term(value)?),
            Some((value, false)) => Bound::Included(term(value)?),
        };
        Ok(Box::new(RangeQuery::new(lower, upper)))
    }

    /// A range on an `integer` column: the ends become an inclusive `i64`
    /// interval, a fraction rounding inwards, so every `i64` is exact.
    fn integer_range(
        &self,
        field: &str,
        lower: Option<&filter::Bound>,
        upper: Option<&filter::Bound>,
    ) -> Result<Box<dyn Query>, Error> {
        let mut lo = i128::from(i64::MIN);
        let mut hi = i128::from(i64::MAX);
        if let Some((value, excluded)) = numeric_end(field, lower)? {
            // A cast from a float to an integer saturates, far outside `i64`.
            lo = lo.max(if excluded {
                value.floor() as i128 + 1
            } else {
                value.ceil() as i128
            });
        }
        if let Some((value, excluded)) = numeric_end(field, upper)? {
            hi = hi.min(if excluded {
                value.ceil() as i128 - 1
            } else {
                value.floor() as i128
            });
        }
        let (Ok(lo), Ok(hi)) = (i64::try_from(lo), i64::try_from(hi)) else {
            return Ok(Box::new(EmptyQuery));
        };
        if lo > hi {
            return Ok(Box::new(EmptyQuery));
        }
        let term = |value: i64| -> Result<Term, Fault> {
            Ok(Term::from_field_text(
                self.fields().num,
                &eq(field, &i64_key(value))?,
            ))
        };
        Ok(Box::new(RangeQuery::new(
            Bound::Included(term(lo)?),
            Bound::Included(term(hi)?),
        )))
    }

    fn boolean(&self, boolean: &filter::Boolean) -> Result<Box<dyn Query>, Error> {
        let mut clauses: Vec<(TantivyOccur, Box<dyn Query>)> =
            Vec::with_capacity(boolean.clauses.len() + 1);
        let mut positive = false;
        for clause in &boolean.clauses {
            let occur = match clause.occur.as_ref().and_then(|occur| occur.as_known()) {
                Some(Occur::OCCUR_MUST) => TantivyOccur::Must,
                Some(Occur::OCCUR_SHOULD) => TantivyOccur::Should,
                Some(Occur::OCCUR_MUST_NOT) => TantivyOccur::MustNot,
                Some(Occur::OCCUR_UNSPECIFIED) | None => {
                    return Err(invalid("a clause of a boolean filter says how it counts").into());
                }
            };
            positive |= occur != TantivyOccur::MustNot;
            let inner = clause
                .query
                .as_option()
                .ok_or_else(|| invalid("a clause of a boolean filter holds no filter"))?;
            clauses.push((occur, self.compile(inner)?));
        }
        // Tantivy matches nothing for clauses that only exclude; "not x" is
        // every Document but x.
        if !positive {
            clauses.push((TantivyOccur::Must, Box::new(AllQuery)));
        }
        let mut query = BooleanQuery::new(clauses);
        if let Some(minimum) = boolean.minimum_should_match.filter(|n| *n > 0) {
            query.set_minimum_number_should_match(usize::try_from(minimum).unwrap_or(usize::MAX));
        }
        Ok(Box::new(query))
    }
}

/// `{column}\0{value}`; `INVALID_ARGUMENT` when either holds a NUL.
fn eq(column: &str, value: &str) -> Result<String, Fault> {
    eq_term(column, value).ok_or_else(|| invalid("a field or a value of the filter holds a NUL"))
}

/// The regular expression over the `tok` terms of `column` for `pattern`.
fn field_pattern(column: &str, pattern: &str) -> String {
    format!("{}\0(?:{pattern})", regex::escape(column))
}

/// The text `value` has in `column`: the term text an `eq` term holds, or
/// `None` for a stored null.
fn column_text(column: &DumpColumn, value: &Value) -> Result<Option<String>, Fault> {
    let wrong = || {
        invalid(format!(
            "the value for the field {} is not a {}",
            column.name,
            match column.kind {
                ColumnKind::Text | ColumnKind::TextList => "string",
                ColumnKind::Integer => "whole number",
                ColumnKind::Number => "number",
                ColumnKind::Boolean => "boolean",
            }
        ))
    };
    match (column.kind, value.kind.as_ref()) {
        (_, Some(ValueKind::NullValue(_))) => Ok(None),
        (ColumnKind::Text | ColumnKind::TextList, Some(ValueKind::StringValue(text))) => {
            Ok(Some(text.clone()))
        }
        (ColumnKind::Integer, Some(ValueKind::NumberValue(n)))
            if n.is_finite() && n.fract() == 0.0 && n.abs() < 9.2e18 =>
        {
            Ok(Some((*n as i64).to_string()))
        }
        (ColumnKind::Number, Some(ValueKind::NumberValue(n))) if n.is_finite() => {
            Ok(Some(n.to_string()))
        }
        (ColumnKind::Boolean, Some(ValueKind::BoolValue(b))) => Ok(Some(b.to_string())),
        _ => Err(wrong()),
    }
}

/// The number an end of a range on the numeric column `field` carries and
/// whether the end is excluded; `None` for an open end.
fn numeric_end(field: &str, end: Option<&filter::Bound>) -> Result<Option<(f64, bool)>, Fault> {
    let Some(end) = end else {
        return Ok(None);
    };
    let (value, excluded) = match end.kind.as_ref() {
        Some(BoundKind::Included(value)) => (value, false),
        Some(BoundKind::Excluded(value)) => (value, true),
        None => return Ok(None),
    };
    match value.kind.as_ref() {
        Some(ValueKind::NumberValue(n)) if n.is_finite() => Ok(Some((*n, excluded))),
        _ => Err(invalid(format!(
            "an end of a range on the numeric field {field} is a finite number"
        ))),
    }
}

/// An end of a range over text: a bound on the terms of `field` (the `id`
/// when `column` is `None`, else the `{column}\0{value}` terms). An open end
/// of a column stays inside the column's terms.
fn text_bound(
    field: Field,
    column: Option<&str>,
    end: Option<&filter::Bound>,
    upper: bool,
) -> Result<Bound<Term>, Fault> {
    let open = || -> Result<Bound<Term>, Fault> {
        match column {
            None => Ok(Bound::Unbounded),
            Some(name) if upper => Ok(Bound::Excluded(Term::from_field_text(
                field,
                &format!("{name}\u{1}"),
            ))),
            Some(name) => Ok(Bound::Included(Term::from_field_text(
                field,
                &eq(name, "")?,
            ))),
        }
    };
    let Some(end) = end else {
        return open();
    };
    let (value, excluded) = match end.kind.as_ref() {
        Some(BoundKind::Included(value)) => (value, false),
        Some(BoundKind::Excluded(value)) => (value, true),
        None => return open(),
    };
    let Some(ValueKind::StringValue(text)) = value.kind.as_ref() else {
        return Err(invalid("an end of a range over text is a string"));
    };
    let text = match column {
        Some(name) => eq(name, text)?,
        None => text.clone(),
    };
    let term = Term::from_field_text(field, &text);
    Ok(if excluded {
        Bound::Excluded(term)
    } else {
        Bound::Included(term)
    })
}
