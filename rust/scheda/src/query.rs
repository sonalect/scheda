//! Map `api.v1.Filter` to a Tantivy 0.26 Query (`13` §3).

use std::ops::Bound as StdBound;

use buffa_types::google::protobuf::value::Kind as ValueKind;
use scheda_generated::api::v1::__buffa::oneof::filter::Kind;
use scheda_generated::api::v1::__buffa::oneof::filter::bound::Kind as BoundKind;
use scheda_generated::api::v1::__buffa::oneof::filter::more_like_this::Source;
use scheda_generated::api::v1::filter::{self, Occur};
use tantivy::Index;
use tantivy::Term;
use tantivy::collector::TopDocs;
use tantivy::query::{
    AllQuery, BooleanQuery, BoostQuery, ConstScoreQuery, DisjunctionMaxQuery, EmptyQuery,
    EnableScoring, FuzzyTermQuery, MoreLikeThisQuery, MoreLikeThisQueryBuilder,
    Occur as TantivyOccur, PhrasePrefixQuery, PhraseQuery, Query, RangeQuery, RegexPhraseQuery,
    RegexQuery, TermQuery, TermSetQuery, Weight,
};
use tantivy::schema::{Field, IndexRecordOption, OwnedValue};
use tantivy::tokenizer::{PreTokenizedString, TextAnalyzer, TokenStream};

use crate::error::{Code, ContextAs, bail};
use crate::schema::{CollectionDecl, FieldDecl};
use crate::{Error, Filter};

pub(crate) struct FindFields {
    pub id: Field,
    pub eq: Field,
    pub exists: Field,
    pub null: Field,
    pub tok: Field,
    pub text: Field,
    pub collection: Field,
}

pub(crate) fn is_unconstrained(filter: &Filter) -> bool {
    matches!(filter.kind.as_ref(), Some(Kind::All(_)))
}

pub(crate) fn validate(decl: &CollectionDecl, filter: &Filter) -> Result<(), Error> {
    filter.require_valid()?;
    check_fields(decl, filter)
}

fn check_fields(decl: &CollectionDecl, filter: &Filter) -> Result<(), Error> {
    let Some(kind) = filter.kind.as_ref() else {
        bail!(Error::invalid("filter kind is required"));
    };
    match kind {
        Kind::All(_) | Kind::Empty(_) => Ok(()),
        Kind::Term(term) => check_leaf(decl, term.field.as_deref().unwrap_or("")),
        Kind::TermSet(term) => check_leaf(decl, term.field.as_deref().unwrap_or("")),
        Kind::Phrase(term) => check_token_field(decl, term.field.as_deref().unwrap_or("")),
        Kind::PhrasePrefix(term) => check_token_field(decl, term.field.as_deref().unwrap_or("")),
        Kind::Fuzzy(term) => check_token_field(decl, term.field.as_deref().unwrap_or("")),
        Kind::Regex(term) => check_token_field(decl, term.field.as_deref().unwrap_or("")),
        Kind::RegexPhrase(term) => check_token_field(decl, term.field.as_deref().unwrap_or("")),
        Kind::Range(term) => check_leaf(decl, term.field.as_deref().unwrap_or("")),
        Kind::Exists(term) => check_leaf(decl, term.field.as_deref().unwrap_or("")),
        Kind::Boolean(boolean) => {
            for clause in &boolean.clauses {
                check_fields(decl, &clause.query)?;
            }
            Ok(())
        }
        Kind::Boost(boost) => check_fields(decl, &boost.query),
        Kind::ConstScore(inner) => check_fields(decl, &inner.query),
        Kind::DisjunctionMax(inner) => {
            for nested in &inner.queries {
                check_fields(decl, nested)?;
            }
            Ok(())
        }
        Kind::MoreLikeThis(mlt) => check_more_like_this(decl, mlt),
    }
}

fn check_more_like_this(decl: &CollectionDecl, mlt: &filter::MoreLikeThis) -> Result<(), Error> {
    match mlt.source.as_ref() {
        Some(Source::Id(_)) | None => Ok(()),
        Some(Source::Fields(fields)) => {
            for field in &fields.fields {
                check_token_field(decl, field.field.as_deref().unwrap_or(""))?;
            }
            Ok(())
        }
    }
}

fn check_token_field(decl: &CollectionDecl, field: &str) -> Result<(), Error> {
    check_leaf(decl, field)?;
    if field == "id" {
        bail!(Error::invalid("this Filter kind cannot use field id"));
    }
    Ok(())
}

fn check_leaf(decl: &CollectionDecl, field: &str) -> Result<(), Error> {
    if field.is_empty() {
        bail!(Error::invalid("filter field is required"));
    }
    if field == "id" {
        return Ok(());
    }
    if !decl.filter_keys.contains(field) {
        bail!(Error::invalid(format!(
            "field {field} is not a find key on {}",
            decl.name
        )));
    }
    Ok(())
}

pub(crate) fn compile(
    decl: &CollectionDecl,
    fields: &FindFields,
    index: &Index,
    filter: &Filter,
) -> Result<Box<dyn Query>, Error> {
    compile_kind(decl, fields, index, filter)
}

fn compile_kind(
    decl: &CollectionDecl,
    fields: &FindFields,
    index: &Index,
    filter: &Filter,
) -> Result<Box<dyn Query>, Error> {
    let Some(kind) = filter.kind.as_ref() else {
        bail!(Error::invalid("filter kind is required"));
    };
    match kind {
        Kind::All(_) => Ok(Box::new(AllQuery)),
        Kind::Empty(_) => Ok(Box::new(EmptyQuery)),
        Kind::Term(term) => compile_term(fields, term),
        Kind::TermSet(term) => compile_term_set(fields, term),
        Kind::Phrase(phrase) => compile_phrase(fields, index, phrase),
        Kind::PhrasePrefix(prefix) => compile_phrase_prefix(fields, index, prefix),
        Kind::Fuzzy(fuzzy) => compile_fuzzy(fields, index, fuzzy),
        Kind::Regex(regex) => compile_regex(fields, regex),
        Kind::RegexPhrase(phrase) => compile_regex_phrase(fields, phrase),
        Kind::Range(range) => compile_range(fields, range),
        Kind::Exists(exists) => compile_exists(decl, fields, exists),
        Kind::Boolean(boolean) => compile_boolean(decl, fields, index, boolean),
        Kind::Boost(boost) => {
            let inner = compile_kind(decl, fields, index, &boost.query)?;
            Ok(Box::new(BoostQuery::new(
                inner,
                boost.factor.unwrap_or(1.0),
            )))
        }
        Kind::ConstScore(inner) => {
            let query = compile_kind(decl, fields, index, &inner.query)?;
            Ok(Box::new(ConstScoreQuery::new(
                query,
                inner.score.unwrap_or(1.0),
            )))
        }
        Kind::DisjunctionMax(inner) => {
            let mut queries = Vec::new();
            for nested in &inner.queries {
                queries.push(compile_kind(decl, fields, index, nested)?);
            }
            Ok(Box::new(DisjunctionMaxQuery::with_tie_breaker(
                queries,
                inner.tie_breaker.unwrap_or(0.0),
            )))
        }
        Kind::MoreLikeThis(mlt) => compile_more_like_this(decl, fields, mlt),
    }
}

fn compile_term(fields: &FindFields, term: &filter::Term) -> Result<Box<dyn Query>, Error> {
    let name = term.field.as_deref().unwrap_or("");
    if name == "id" {
        let Some(id) = proto_string(&term.value) else {
            if is_proto_null(&term.value) {
                bail!(Error::invalid("term id cannot be null"));
            }
            bail!(Error::invalid("term id must be a string"));
        };
        return Ok(Box::new(TermQuery::new(
            Term::from_field_text(fields.id, &id),
            IndexRecordOption::Basic,
        )));
    }
    if is_proto_null(&term.value) {
        return Ok(Box::new(TermQuery::new(
            Term::from_field_text(fields.null, name),
            IndexRecordOption::Basic,
        )));
    }
    let value = proto_term_text(&term.value)?;
    Ok(Box::new(TermQuery::new(
        Term::from_field_text(fields.eq, &eq_term(name, &value)?),
        IndexRecordOption::Basic,
    )))
}

fn compile_term_set(fields: &FindFields, term: &filter::TermSet) -> Result<Box<dyn Query>, Error> {
    let name = term.field.as_deref().unwrap_or("");
    let mut terms = Vec::new();
    for value in &term.values.values {
        if name == "id" {
            let Some(id) = proto_string(value) else {
                bail!(Error::invalid("term_set id values must be strings"));
            };
            terms.push(Term::from_field_text(fields.id, &id));
        } else if is_proto_null(value) {
            terms.push(Term::from_field_text(fields.null, name));
        } else {
            let text = proto_term_text(value)?;
            terms.push(Term::from_field_text(fields.eq, &eq_term(name, &text)?));
        }
    }
    Ok(Box::new(TermSetQuery::new(terms)))
}

fn compile_exists(
    decl: &CollectionDecl,
    fields: &FindFields,
    exists: &filter::Exists,
) -> Result<Box<dyn Query>, Error> {
    let name = exists.field.as_deref().unwrap_or("");
    if name == "id" {
        return Ok(Box::new(AllQuery));
    }
    let mut names = vec![name.to_owned()];
    if exists.json_subpaths.unwrap_or(false) {
        names.extend(descendant_filter_keys(&decl.fields, name));
        names.sort_unstable();
        names.dedup();
    }
    match names.as_slice() {
        [] => unreachable!("exists field name is required"),
        [one] => Ok(Box::new(TermQuery::new(
            Term::from_field_text(fields.exists, one),
            IndexRecordOption::Basic,
        ))),
        many => {
            let clauses = many
                .iter()
                .map(|n| {
                    (
                        TantivyOccur::Should,
                        Box::new(TermQuery::new(
                            Term::from_field_text(fields.exists, n),
                            IndexRecordOption::Basic,
                        )) as Box<dyn Query>,
                    )
                })
                .collect();
            Ok(Box::new(BooleanQuery::new(clauses)))
        }
    }
}

fn descendant_filter_keys(fields: &[FieldDecl], name: &str) -> Vec<String> {
    fn find<'a>(fields: &'a [FieldDecl], name: &str) -> Option<&'a FieldDecl> {
        for field in fields {
            if field.name == name {
                return Some(field);
            }
            if let Some(found) = find(&field.fields, name) {
                return Some(found);
            }
        }
        None
    }
    fn collect(fields: &[FieldDecl], out: &mut Vec<String>) {
        for field in fields {
            if field.filter {
                out.push(field.name.clone());
            }
            collect(&field.fields, out);
        }
    }
    let Some(field) = find(fields, name) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect(&field.fields, &mut out);
    out
}

fn compile_phrase(
    fields: &FindFields,
    index: &Index,
    phrase: &filter::Phrase,
) -> Result<Box<dyn Query>, Error> {
    let name = phrase.field.as_deref().unwrap_or("");
    let mut tokens = Vec::new();
    for term in &phrase.terms {
        tokens.extend(analyze_tokens(index, fields.tok, term)?);
    }
    if tokens.len() < 2 {
        bail!(Error::invalid("phrase needs at least two indexed tokens"));
    }
    let terms: Vec<Term> = tokens
        .iter()
        .map(|token| eq_term(name, token).map(|text| Term::from_field_text(fields.tok, &text)))
        .collect::<Result<Vec<_>, _>>()?;
    let mut query = PhraseQuery::new(terms);
    if let Some(slop) = phrase.slop {
        query.set_slop(slop);
    }
    Ok(Box::new(query))
}

fn compile_phrase_prefix(
    fields: &FindFields,
    index: &Index,
    prefix: &filter::PhrasePrefix,
) -> Result<Box<dyn Query>, Error> {
    let name = prefix.field.as_deref().unwrap_or("");
    let mut tokens = Vec::new();
    for term in &prefix.terms {
        tokens.extend(analyze_tokens(index, fields.tok, term)?);
    }
    let prefix_text = prefix.prefix.as_deref().unwrap_or("");
    let mut prefix_tokens = analyze_tokens(index, fields.tok, prefix_text)?;
    if prefix_tokens.is_empty() {
        prefix_tokens.push(prefix_text.to_lowercase());
    }
    tokens.extend(prefix_tokens);
    if tokens.len() < 2 {
        bail!(Error::invalid(
            "phrase_prefix needs a complete token and a prefix"
        ));
    }
    let terms: Vec<Term> = tokens
        .iter()
        .map(|token| eq_term(name, token).map(|text| Term::from_field_text(fields.tok, &text)))
        .collect::<Result<Vec<_>, _>>()?;
    let mut query = PhrasePrefixQuery::new(terms);
    if let Some(max) = prefix.max_expansions.filter(|n| *n > 0) {
        query.set_max_expansions(max);
    }
    Ok(Box::new(query))
}

fn compile_fuzzy(
    fields: &FindFields,
    index: &Index,
    fuzzy: &filter::FuzzyTerm,
) -> Result<Box<dyn Query>, Error> {
    let name = fuzzy.field.as_deref().unwrap_or("");
    let raw = fuzzy.term.as_deref().unwrap_or("");
    let tokens = analyze_tokens(index, fields.tok, raw)?;
    let token = match tokens.as_slice() {
        [one] => one.as_str(),
        [] => raw,
        _ => bail!(Error::invalid("fuzzy term must be a single token")),
    };
    if token.is_empty() {
        bail!(Error::invalid("fuzzy term is empty"));
    }
    let term = Term::from_field_text(fields.tok, &eq_term(name, token)?);
    let distance = u8::try_from(fuzzy.distance.unwrap_or(0)).unwrap_or(u8::MAX);
    let transposition = fuzzy.transposition_cost_one.unwrap_or(false);
    if fuzzy.prefix.unwrap_or(false) {
        Ok(Box::new(FuzzyTermQuery::new_prefix(
            term,
            distance,
            transposition,
        )))
    } else {
        Ok(Box::new(FuzzyTermQuery::new(term, distance, transposition)))
    }
}

fn compile_regex(fields: &FindFields, regex: &filter::Regex) -> Result<Box<dyn Query>, Error> {
    let name = regex.field.as_deref().unwrap_or("");
    let pattern = regex.pattern.as_deref().unwrap_or("");
    let anchored = field_token_regex(name, pattern);
    RegexQuery::from_pattern(&anchored, fields.tok)
        .context(Code::Invalid, "[Filter] regex")
        .map(|query| Box::new(query) as Box<dyn Query>)
}

fn compile_regex_phrase(
    fields: &FindFields,
    phrase: &filter::RegexPhrase,
) -> Result<Box<dyn Query>, Error> {
    let name = phrase.field.as_deref().unwrap_or("");
    let patterns: Vec<String> = phrase
        .patterns
        .iter()
        .map(|pattern| field_token_regex(name, pattern))
        .collect();
    let mut query = RegexPhraseQuery::new(fields.tok, patterns);
    if let Some(slop) = phrase.slop {
        query.set_slop(slop);
    }
    if let Some(max) = phrase.max_expansions.filter(|n| *n > 0) {
        query.set_max_expansions(max);
    }
    Ok(Box::new(query))
}

fn compile_range(fields: &FindFields, range: &filter::Range) -> Result<Box<dyn Query>, Error> {
    let name = range.field.as_deref().unwrap_or("");
    if name == "id" {
        let lower = range_bound(fields.id, None, range.lower.as_option(), false)?;
        let upper = range_bound(fields.id, None, range.upper.as_option(), true)?;
        return Ok(Box::new(RangeQuery::new(lower, upper)));
    }
    let lower = range_bound(fields.eq, Some(name), range.lower.as_option(), false)?;
    let upper = range_bound(fields.eq, Some(name), range.upper.as_option(), true)?;
    Ok(Box::new(RangeQuery::new(lower, upper)))
}

fn range_bound(
    field: Field,
    prefix: Option<&str>,
    bound: Option<&filter::Bound>,
    upper: bool,
) -> Result<StdBound<Term>, Error> {
    let Some(bound) = bound else {
        return unbounded_field_bound(field, prefix, upper);
    };
    match bound.kind.as_ref() {
        Some(BoundKind::Included(value)) => {
            Ok(StdBound::Included(range_term(field, prefix, value)?))
        }
        Some(BoundKind::Excluded(value)) => {
            Ok(StdBound::Excluded(range_term(field, prefix, value)?))
        }
        None => Ok(unbounded_field_bound(field, prefix, upper)?),
    }
}

fn unbounded_field_bound(
    field: Field,
    prefix: Option<&str>,
    upper: bool,
) -> Result<StdBound<Term>, Error> {
    match prefix {
        None => Ok(StdBound::Unbounded),
        Some(name) if upper => Ok(StdBound::Excluded(Term::from_field_text(
            field,
            &format!("{name}\u{0001}"),
        ))),
        Some(name) => Ok(StdBound::Included(Term::from_field_text(
            field,
            &eq_term(name, "")?,
        ))),
    }
}

fn range_term(
    field: Field,
    prefix: Option<&str>,
    value: &buffa_types::google::protobuf::Value,
) -> Result<Term, Error> {
    let text = proto_term_text(value)?;
    Ok(match prefix {
        Some(name) => Term::from_field_text(field, &eq_term(name, &text)?),
        None => Term::from_field_text(field, &text),
    })
}

fn compile_boolean(
    decl: &CollectionDecl,
    fields: &FindFields,
    index: &Index,
    boolean: &filter::Boolean,
) -> Result<Box<dyn Query>, Error> {
    let mut clauses = Vec::new();
    for clause in &boolean.clauses {
        let occur = match clause.occur {
            Some(ref value) if *value == Occur::OCCUR_MUST || *value == Occur::Must => {
                TantivyOccur::Must
            }
            Some(ref value) if *value == Occur::OCCUR_SHOULD || *value == Occur::Should => {
                TantivyOccur::Should
            }
            Some(ref value) if *value == Occur::OCCUR_MUST_NOT || *value == Occur::MustNot => {
                TantivyOccur::MustNot
            }
            _ => bail!(Error::invalid("boolean clause occur is required")),
        };
        clauses.push((occur, compile_kind(decl, fields, index, &clause.query)?));
    }
    let mut query = BooleanQuery::new(clauses);
    if let Some(minimum) = boolean.minimum_should_match.filter(|n| *n > 0) {
        query.set_minimum_number_should_match(minimum as usize);
    }
    Ok(Box::new(query))
}

fn compile_more_like_this(
    decl: &CollectionDecl,
    fields: &FindFields,
    mlt: &filter::MoreLikeThis,
) -> Result<Box<dyn Query>, Error> {
    let builder = more_like_this_builder(mlt)?;
    match mlt.source.as_ref() {
        Some(Source::Id(record_id)) => Ok(Box::new(MoreLikeThisIdQuery {
            collection: decl.name.clone(),
            record_id: record_id.clone(),
            f_collection: fields.collection,
            f_id: fields.id,
            builder,
        })),
        Some(Source::Fields(source)) => {
            let mut values = Vec::new();
            for field in &source.fields {
                if !field.values.is_set() {
                    continue;
                }
                for value in &field.values.values {
                    values.push(OwnedValue::Str(proto_term_text(value)?));
                }
            }
            if values.is_empty() {
                return Ok(Box::new(EmptyQuery));
            }
            Ok(Box::new(
                builder.with_document_fields(vec![(fields.text, values)]),
            ))
        }
        None => Ok(Box::new(EmptyQuery)),
    }
}

fn more_like_this_builder(mlt: &filter::MoreLikeThis) -> Result<MoreLikeThisQueryBuilder, Error> {
    let mut builder = MoreLikeThisQuery::builder();
    if let Some(n) = mlt.min_doc_frequency {
        builder = builder.with_min_doc_frequency(n);
    }
    if let Some(n) = mlt.max_doc_frequency {
        builder = builder.with_max_doc_frequency(n);
    }
    if let Some(n) = mlt.min_term_frequency {
        builder = builder.with_min_term_frequency(usize_cap(n, "min_term_frequency")?);
    }
    if let Some(n) = mlt.max_query_terms {
        builder = builder.with_max_query_terms(usize_cap(n, "max_query_terms")?);
    }
    if let Some(n) = mlt.min_word_length {
        builder = builder.with_min_word_length(usize_cap(n, "min_word_length")?);
    }
    if let Some(n) = mlt.max_word_length {
        builder = builder.with_max_word_length(usize_cap(n, "max_word_length")?);
    }
    if let Some(factor) = mlt.boost_factor {
        builder = builder.with_boost_factor(factor);
    }
    if !mlt.stop_words.is_empty() {
        builder = builder.with_stop_words(mlt.stop_words.clone());
    }
    Ok(builder)
}

fn usize_cap(value: u64, name: &str) -> Result<usize, Error> {
    usize::try_from(value).map_err(|_| Error::invalid(format!("MoreLikeThis {name} is too large")))
}

#[derive(Debug, Clone)]
struct MoreLikeThisIdQuery {
    collection: String,
    record_id: String,
    f_collection: Field,
    f_id: Field,
    builder: MoreLikeThisQueryBuilder,
}

impl Query for MoreLikeThisIdQuery {
    fn weight(&self, enable_scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        let searcher = match enable_scoring {
            EnableScoring::Enabled { searcher, .. } => searcher,
            EnableScoring::Disabled { .. } => {
                return Err(tantivy::TantivyError::InvalidArgument(
                    "MoreLikeThisQuery requires to enable scoring.".to_string(),
                ));
            }
        };
        let lookup = BooleanQuery::new(vec![
            (
                TantivyOccur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.f_collection, &self.collection),
                    IndexRecordOption::Basic,
                )) as Box<dyn Query>,
            ),
            (
                TantivyOccur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.f_id, &self.record_id),
                    IndexRecordOption::Basic,
                )),
            ),
        ]);
        let hits = searcher.search(&lookup, &TopDocs::with_limit(1).order_by_score())?;
        let Some((_, addr)) = hits.into_iter().next() else {
            return EmptyQuery.weight(enable_scoring);
        };
        self.builder
            .clone()
            .with_document(addr)
            .weight(enable_scoring)
    }
}

pub(crate) fn eq_term(field: &str, value: &str) -> Result<String, Error> {
    if field.contains('\0') || value.contains('\0') {
        // Indexed as `{field}\0{value}` in one STRING field.
        return Err(Error::invalid(
            "filter field or value must not contain NUL",
        ));
    }
    Ok(format!("{field}\0{value}"))
}

pub(crate) fn add_field_tokens(
    doc: &mut tantivy::TantivyDocument,
    f_tok: Field,
    field_name: &str,
    value: &str,
    analyzer: &mut TextAnalyzer,
) -> Result<(), Error> {
    let mut stream = analyzer.token_stream(value);
    let mut tokens = Vec::new();
    let mut position = 0usize;
    while stream.advance() {
        let token = stream.token();
        tokens.push(tantivy::tokenizer::Token {
            offset_from: token.offset_from,
            offset_to: token.offset_to,
            position,
            text: eq_term(field_name, &token.text)?,
            position_length: 1,
        });
        position += 1;
    }
    if tokens.is_empty() {
        return Ok(());
    }
    doc.add_pre_tokenized_text(
        f_tok,
        PreTokenizedString {
            text: value.to_owned(),
            tokens,
        },
    );
    Ok(())
}

fn analyze_tokens(index: &Index, field: Field, text: &str) -> Result<Vec<String>, Error> {
    let mut analyzer = index
        .tokenizer_for_field(field)
        .context(Code::IndexLexical, "[Filter] tokenizer")?;
    let mut stream = analyzer.token_stream(text);
    let mut tokens = Vec::new();
    while stream.advance() {
        tokens.push(stream.token().text.clone());
    }
    Ok(tokens)
}

fn field_token_regex(field: &str, pattern: &str) -> String {
    format!("{}{}(?:{})", regex::escape(field), '\0', pattern)
}

fn proto_string(value: &buffa_types::google::protobuf::Value) -> Option<String> {
    match value.kind.as_ref() {
        Some(ValueKind::StringValue(text)) => Some(text.clone()),
        _ => None,
    }
}

fn is_proto_null(value: &buffa_types::google::protobuf::Value) -> bool {
    matches!(value.kind.as_ref(), Some(ValueKind::NullValue(_)))
}

fn proto_term_text(value: &buffa_types::google::protobuf::Value) -> Result<String, Error> {
    match value.kind.as_ref() {
        Some(ValueKind::StringValue(text)) => Ok(text.clone()),
        Some(ValueKind::NumberValue(n)) => Ok(n.to_string()),
        Some(ValueKind::BoolValue(b)) => Ok(b.to_string()),
        Some(ValueKind::NullValue(_)) => bail!(Error::invalid("null term uses the null index")),
        _ => bail!(Error::invalid("filter term value is not a scalar")),
    }
}
