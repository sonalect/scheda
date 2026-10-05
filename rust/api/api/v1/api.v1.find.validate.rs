use super::*;
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for Filter {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        match &self.kind {
            Some(__buffa::oneof::filter::Kind::All(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(1i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("all")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::oneof::filter::Kind::Empty(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(2i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("empty")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::oneof::filter::Kind::Term(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(3i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("term")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::oneof::filter::Kind::TermSet(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(4i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("term_set")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::oneof::filter::Kind::Phrase(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(5i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("phrase")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::oneof::filter::Kind::PhrasePrefix(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(6i32),
                                                field_name: Some(
                                                    ::std::borrow::Cow::Borrowed("phrase_prefix"),
                                                ),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::oneof::filter::Kind::Fuzzy(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(7i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("fuzzy")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::oneof::filter::Kind::Regex(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(8i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("regex")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::oneof::filter::Kind::RegexPhrase(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(9i32),
                                                field_name: Some(
                                                    ::std::borrow::Cow::Borrowed("regex_phrase"),
                                                ),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::oneof::filter::Kind::Range(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(10i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("range")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::oneof::filter::Kind::Exists(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(11i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("exists")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::oneof::filter::Kind::Boolean(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(12i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("boolean")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::oneof::filter::Kind::Boost(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(13i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("boost")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::oneof::filter::Kind::ConstScore(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(14i32),
                                                field_name: Some(
                                                    ::std::borrow::Cow::Borrowed("const_score"),
                                                ),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::oneof::filter::Kind::DisjunctionMax(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(15i32),
                                                field_name: Some(
                                                    ::std::borrow::Cow::Borrowed("disjunction_max"),
                                                ),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::oneof::filter::Kind::MoreLikeThis(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(16i32),
                                                field_name: Some(
                                                    ::std::borrow::Cow::Borrowed("more_like_this"),
                                                ),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            None => {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                None, field_name :
                                Some(::std::borrow::Cow::Borrowed("kind")), field_type :
                                None, key_type : None, value_type : None, subscript : None,
                                },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath::default(),
                        rule_id: ::std::borrow::Cow::Borrowed("required"),
                        message: ::std::borrow::Cow::Borrowed(
                            "at least one variant is required",
                        ),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for __buffa::view::FilterView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        match &self.kind {
            Some(__buffa::view::oneof::filter::Kind::All(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(1i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("all")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::view::oneof::filter::Kind::Empty(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(2i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("empty")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::view::oneof::filter::Kind::Term(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(3i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("term")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::view::oneof::filter::Kind::TermSet(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(4i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("term_set")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::view::oneof::filter::Kind::Phrase(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(5i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("phrase")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::view::oneof::filter::Kind::PhrasePrefix(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(6i32),
                                                field_name: Some(
                                                    ::std::borrow::Cow::Borrowed("phrase_prefix"),
                                                ),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::view::oneof::filter::Kind::Fuzzy(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(7i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("fuzzy")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::view::oneof::filter::Kind::Regex(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(8i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("regex")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::view::oneof::filter::Kind::RegexPhrase(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(9i32),
                                                field_name: Some(
                                                    ::std::borrow::Cow::Borrowed("regex_phrase"),
                                                ),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::view::oneof::filter::Kind::Range(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(10i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("range")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::view::oneof::filter::Kind::Exists(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(11i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("exists")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::view::oneof::filter::Kind::Boolean(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(12i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("boolean")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::view::oneof::filter::Kind::Boost(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(13i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("boost")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::view::oneof::filter::Kind::ConstScore(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(14i32),
                                                field_name: Some(
                                                    ::std::borrow::Cow::Borrowed("const_score"),
                                                ),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::view::oneof::filter::Kind::DisjunctionMax(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(15i32),
                                                field_name: Some(
                                                    ::std::borrow::Cow::Borrowed("disjunction_max"),
                                                ),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            Some(__buffa::view::oneof::filter::Kind::MoreLikeThis(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(16i32),
                                                field_name: Some(
                                                    ::std::borrow::Cow::Borrowed("more_like_this"),
                                                ),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            None => {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                None, field_name :
                                Some(::std::borrow::Cow::Borrowed("kind")), field_type :
                                None, key_type : None, value_type : None, subscript : None,
                                },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath::default(),
                        rule_id: ::std::borrow::Cow::Borrowed("required"),
                        message: ::std::borrow::Cow::Borrowed(
                            "at least one variant is required",
                        ),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for filter::All {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for __buffa::view::filter::AllView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for filter::Empty {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for __buffa::view::filter::EmptyView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for filter::Term {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.field.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("field")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.field.is_some() {
            if let Some(v) = self.field.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("field")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if !self.value.is_set() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(2i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("value")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.value.is_set() {}
        if let Some(v) = self.collection.as_deref() {
            if v.chars().count() < 1usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(3i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("collection")), field_type
                                : Some(::protovalidate_buffa::FieldType::String), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(14i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for __buffa::view::filter::TermView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.field.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("field")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.field.is_some() {
            if let Some(v) = self.field.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("field")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if !self.value.is_set() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(2i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("value")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.value.is_set() {}
        if let Some(v) = self.collection.as_deref() {
            if v.chars().count() < 1usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(3i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("collection")), field_type
                                : Some(::protovalidate_buffa::FieldType::String), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(14i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for filter::TermSet {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.field.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("field")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.field.is_some() {
            if let Some(v) = self.field.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("field")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if !self.values.is_set() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(2i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("values")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.values.is_set() {}
        if let Some(v) = self.collection.as_deref() {
            if v.chars().count() < 1usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(3i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("collection")), field_type
                                : Some(::protovalidate_buffa::FieldType::String), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(14i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for __buffa::view::filter::TermSetView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.field.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("field")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.field.is_some() {
            if let Some(v) = self.field.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("field")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if !self.values.is_set() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(2i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("values")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.values.is_set() {}
        if let Some(v) = self.collection.as_deref() {
            if v.chars().count() < 1usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(3i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("collection")), field_type
                                : Some(::protovalidate_buffa::FieldType::String), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(14i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for filter::Phrase {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.field.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("field")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.field.is_some() {
            if let Some(v) = self.field.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("field")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if self.terms.len() < 2usize {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(2i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("terms")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(18i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("repeated")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("min_items")), field_type :
                            Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("repeated.min_items"),
                    message: ::std::borrow::Cow::Owned(
                        ::std::format!(
                            "list must contain at least {} items (got {})", 2usize, self
                            .terms.len()
                        ),
                    ),
                    for_key: false,
                });
        }
        if let Some(v) = self.collection.as_deref() {
            if v.chars().count() < 1usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(4i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("collection")), field_type
                                : Some(::protovalidate_buffa::FieldType::String), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(14i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for __buffa::view::filter::PhraseView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.field.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("field")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.field.is_some() {
            if let Some(v) = self.field.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("field")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if self.terms.len() < 2usize {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(2i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("terms")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(18i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("repeated")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("min_items")), field_type :
                            Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("repeated.min_items"),
                    message: ::std::borrow::Cow::Owned(
                        ::std::format!(
                            "list must contain at least {} items (got {})", 2usize, self
                            .terms.len()
                        ),
                    ),
                    for_key: false,
                });
        }
        if let Some(v) = self.collection.as_deref() {
            if v.chars().count() < 1usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(4i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("collection")), field_type
                                : Some(::protovalidate_buffa::FieldType::String), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(14i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for filter::PhrasePrefix {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.field.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("field")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.field.is_some() {
            if let Some(v) = self.field.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("field")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if self.terms.len() < 1usize {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(2i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("terms")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(18i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("repeated")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("min_items")), field_type :
                            Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("repeated.min_items"),
                    message: ::std::borrow::Cow::Owned(
                        ::std::format!(
                            "list must contain at least {} items (got {})", 1usize, self
                            .terms.len()
                        ),
                    ),
                    for_key: false,
                });
        }
        if self.prefix.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(3i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("prefix")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.prefix.is_some() {
            if let Some(v) = self.prefix.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(3i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("prefix")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if let Some(v) = self.collection.as_deref() {
            if v.chars().count() < 1usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("collection")), field_type
                                : Some(::protovalidate_buffa::FieldType::String), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(14i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for __buffa::view::filter::PhrasePrefixView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.field.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("field")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.field.is_some() {
            if let Some(v) = self.field.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("field")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if self.terms.len() < 1usize {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(2i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("terms")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(18i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("repeated")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("min_items")), field_type :
                            Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("repeated.min_items"),
                    message: ::std::borrow::Cow::Owned(
                        ::std::format!(
                            "list must contain at least {} items (got {})", 1usize, self
                            .terms.len()
                        ),
                    ),
                    for_key: false,
                });
        }
        if self.prefix.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(3i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("prefix")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.prefix.is_some() {
            if let Some(v) = self.prefix.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(3i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("prefix")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if let Some(v) = self.collection.as_deref() {
            if v.chars().count() < 1usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("collection")), field_type
                                : Some(::protovalidate_buffa::FieldType::String), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(14i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for filter::FuzzyTerm {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.field.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("field")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.field.is_some() {
            if let Some(v) = self.field.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("field")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if self.term.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(2i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("term")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.term.is_some() {
            if let Some(v) = self.term.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("term")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if let Some(v) = self.distance {
            if v > 2u32 {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(3i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("distance")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint32), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("uint32")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(3i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("lte")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint32), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("uint32.lte"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(v) = self.collection.as_deref() {
            if v.chars().count() < 1usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(6i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("collection")), field_type
                                : Some(::protovalidate_buffa::FieldType::String), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(14i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for __buffa::view::filter::FuzzyTermView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.field.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("field")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.field.is_some() {
            if let Some(v) = self.field.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("field")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if self.term.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(2i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("term")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.term.is_some() {
            if let Some(v) = self.term.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("term")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if let Some(v) = self.distance {
            if v > 2u32 {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(3i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("distance")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint32), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("uint32")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(3i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("lte")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint32), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("uint32.lte"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(v) = self.collection.as_deref() {
            if v.chars().count() < 1usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(6i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("collection")), field_type
                                : Some(::protovalidate_buffa::FieldType::String), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(14i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for filter::Regex {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.field.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("field")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.field.is_some() {
            if let Some(v) = self.field.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("field")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if self.pattern.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(2i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("pattern")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.pattern.is_some() {
            if let Some(v) = self.pattern.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("pattern")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if let Some(v) = self.collection.as_deref() {
            if v.chars().count() < 1usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(3i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("collection")), field_type
                                : Some(::protovalidate_buffa::FieldType::String), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(14i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for __buffa::view::filter::RegexView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.field.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("field")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.field.is_some() {
            if let Some(v) = self.field.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("field")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if self.pattern.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(2i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("pattern")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.pattern.is_some() {
            if let Some(v) = self.pattern.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("pattern")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if let Some(v) = self.collection.as_deref() {
            if v.chars().count() < 1usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(3i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("collection")), field_type
                                : Some(::protovalidate_buffa::FieldType::String), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(14i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for filter::RegexPhrase {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.field.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("field")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.field.is_some() {
            if let Some(v) = self.field.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("field")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if self.patterns.len() < 2usize {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(2i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("patterns")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(18i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("repeated")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("min_items")), field_type :
                            Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("repeated.min_items"),
                    message: ::std::borrow::Cow::Owned(
                        ::std::format!(
                            "list must contain at least {} items (got {})", 2usize, self
                            .patterns.len()
                        ),
                    ),
                    for_key: false,
                });
        }
        if let Some(v) = self.collection.as_deref() {
            if v.chars().count() < 1usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("collection")), field_type
                                : Some(::protovalidate_buffa::FieldType::String), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(14i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for __buffa::view::filter::RegexPhraseView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.field.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("field")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.field.is_some() {
            if let Some(v) = self.field.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("field")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if self.patterns.len() < 2usize {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(2i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("patterns")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(18i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("repeated")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("min_items")), field_type :
                            Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("repeated.min_items"),
                    message: ::std::borrow::Cow::Owned(
                        ::std::format!(
                            "list must contain at least {} items (got {})", 2usize, self
                            .patterns.len()
                        ),
                    ),
                    for_key: false,
                });
        }
        if let Some(v) = self.collection.as_deref() {
            if v.chars().count() < 1usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("collection")), field_type
                                : Some(::protovalidate_buffa::FieldType::String), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(14i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for filter::Bound {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.kind.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            None, field_name :
                            Some(::std::borrow::Cow::Borrowed("kind")), field_type :
                            None, key_type : None, value_type : None, subscript : None,
                            },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath::default(),
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed(
                        "at least one variant is required",
                    ),
                    for_key: false,
                });
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for __buffa::view::filter::BoundView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.kind.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            None, field_name :
                            Some(::std::borrow::Cow::Borrowed("kind")), field_type :
                            None, key_type : None, value_type : None, subscript : None,
                            },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath::default(),
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed(
                        "at least one variant is required",
                    ),
                    for_key: false,
                });
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for filter::Range {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.field.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("field")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.field.is_some() {
            if let Some(v) = self.field.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("field")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if let Some(inner) = self.lower.as_option() {
            if let Err(sub) = inner.validate() {
                violations
                    .extend(
                        sub
                            .violations
                            .into_iter()
                            .map(|mut v| {
                                v.field
                                    .elements
                                    .insert(
                                        0,
                                        ::protovalidate_buffa::FieldPathElement {
                                            field_number: Some(2i32),
                                            field_name: Some(::std::borrow::Cow::Borrowed("lower")),
                                            field_type: Some(::protovalidate_buffa::FieldType::Message),
                                            key_type: None,
                                            value_type: None,
                                            subscript: None,
                                        },
                                    );
                                v
                            }),
                    );
            }
        }
        if let Some(inner) = self.upper.as_option() {
            if let Err(sub) = inner.validate() {
                violations
                    .extend(
                        sub
                            .violations
                            .into_iter()
                            .map(|mut v| {
                                v.field
                                    .elements
                                    .insert(
                                        0,
                                        ::protovalidate_buffa::FieldPathElement {
                                            field_number: Some(3i32),
                                            field_name: Some(::std::borrow::Cow::Borrowed("upper")),
                                            field_type: Some(::protovalidate_buffa::FieldType::Message),
                                            key_type: None,
                                            value_type: None,
                                            subscript: None,
                                        },
                                    );
                                v
                            }),
                    );
            }
        }
        if let Some(v) = self.collection.as_deref() {
            if v.chars().count() < 1usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(4i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("collection")), field_type
                                : Some(::protovalidate_buffa::FieldType::String), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(14i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for __buffa::view::filter::RangeView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.field.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("field")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.field.is_some() {
            if let Some(v) = self.field.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("field")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if let Some(inner) = self.lower.as_option() {
            if let Err(sub) = inner.validate() {
                violations
                    .extend(
                        sub
                            .violations
                            .into_iter()
                            .map(|mut v| {
                                v.field
                                    .elements
                                    .insert(
                                        0,
                                        ::protovalidate_buffa::FieldPathElement {
                                            field_number: Some(2i32),
                                            field_name: Some(::std::borrow::Cow::Borrowed("lower")),
                                            field_type: Some(::protovalidate_buffa::FieldType::Message),
                                            key_type: None,
                                            value_type: None,
                                            subscript: None,
                                        },
                                    );
                                v
                            }),
                    );
            }
        }
        if let Some(inner) = self.upper.as_option() {
            if let Err(sub) = inner.validate() {
                violations
                    .extend(
                        sub
                            .violations
                            .into_iter()
                            .map(|mut v| {
                                v.field
                                    .elements
                                    .insert(
                                        0,
                                        ::protovalidate_buffa::FieldPathElement {
                                            field_number: Some(3i32),
                                            field_name: Some(::std::borrow::Cow::Borrowed("upper")),
                                            field_type: Some(::protovalidate_buffa::FieldType::Message),
                                            key_type: None,
                                            value_type: None,
                                            subscript: None,
                                        },
                                    );
                                v
                            }),
                    );
            }
        }
        if let Some(v) = self.collection.as_deref() {
            if v.chars().count() < 1usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(4i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("collection")), field_type
                                : Some(::protovalidate_buffa::FieldType::String), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(14i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for filter::Exists {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.field.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("field")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.field.is_some() {
            if let Some(v) = self.field.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("field")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if let Some(v) = self.collection.as_deref() {
            if v.chars().count() < 1usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(3i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("collection")), field_type
                                : Some(::protovalidate_buffa::FieldType::String), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(14i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for __buffa::view::filter::ExistsView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.field.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("field")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.field.is_some() {
            if let Some(v) = self.field.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("field")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if let Some(v) = self.collection.as_deref() {
            if v.chars().count() < 1usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(3i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("collection")), field_type
                                : Some(::protovalidate_buffa::FieldType::String), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(14i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for filter::Clause {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if !self.query.is_set() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(2i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("query")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.query.is_set() {
            if let Some(inner) = self.query.as_option() {
                if let Err(sub) = inner.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(2i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("query")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for __buffa::view::filter::ClauseView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if !self.query.is_set() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(2i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("query")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.query.is_set() {
            if let Some(inner) = self.query.as_option() {
                if let Err(sub) = inner.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(2i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("query")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for filter::Boolean {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.clauses.len() < 1usize {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("clauses")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(18i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("repeated")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("min_items")), field_type :
                            Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("repeated.min_items"),
                    message: ::std::borrow::Cow::Owned(
                        ::std::format!(
                            "list must contain at least {} items (got {})", 1usize, self
                            .clauses.len()
                        ),
                    ),
                    for_key: false,
                });
        }
        for (idx, elem) in self.clauses.iter().enumerate() {
            if let Err(sub) = elem.validate() {
                violations
                    .extend(
                        sub
                            .violations
                            .into_iter()
                            .map(|mut v| {
                                v.field
                                    .elements
                                    .insert(
                                        0,
                                        ::protovalidate_buffa::FieldPathElement {
                                            field_number: Some(1i32),
                                            field_name: Some(::std::borrow::Cow::Borrowed("clauses")),
                                            field_type: Some(::protovalidate_buffa::FieldType::Message),
                                            key_type: None,
                                            value_type: None,
                                            subscript: Some(
                                                ::protovalidate_buffa::Subscript::Index(idx as u64),
                                            ),
                                        },
                                    );
                                v
                            }),
                    );
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for __buffa::view::filter::BooleanView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.clauses.len() < 1usize {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("clauses")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(18i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("repeated")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("min_items")), field_type :
                            Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("repeated.min_items"),
                    message: ::std::borrow::Cow::Owned(
                        ::std::format!(
                            "list must contain at least {} items (got {})", 1usize, self
                            .clauses.len()
                        ),
                    ),
                    for_key: false,
                });
        }
        for (idx, elem) in self.clauses.iter().enumerate() {
            if let Err(sub) = elem.validate() {
                violations
                    .extend(
                        sub
                            .violations
                            .into_iter()
                            .map(|mut v| {
                                v.field
                                    .elements
                                    .insert(
                                        0,
                                        ::protovalidate_buffa::FieldPathElement {
                                            field_number: Some(1i32),
                                            field_name: Some(::std::borrow::Cow::Borrowed("clauses")),
                                            field_type: Some(::protovalidate_buffa::FieldType::Message),
                                            key_type: None,
                                            value_type: None,
                                            subscript: Some(
                                                ::protovalidate_buffa::Subscript::Index(idx as u64),
                                            ),
                                        },
                                    );
                                v
                            }),
                    );
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for filter::Boost {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if !self.query.is_set() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("query")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.query.is_set() {
            if let Some(inner) = self.query.as_option() {
                if let Err(sub) = inner.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(1i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("query")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
        }
        if let ::core::option::Option::Some(__cel_inner) = self.factor.as_ref() {
            let __cel_this = ((*__cel_inner) as f64);
            if !(((!(((__cel_this))).is_nan()) && (!(((__cel_this))).is_infinite()))) {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("factor")), field_type :
                                Some(::protovalidate_buffa::FieldType::Float), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(23), field_name :
                                Some(::std::borrow::Cow::Borrowed("cel")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript :
                                Some(::protovalidate_buffa::Subscript::Index(0u64)), }
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("filter.boost.finite"),
                        message: ::std::borrow::Cow::Borrowed("must be finite"),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for __buffa::view::filter::BoostView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if !self.query.is_set() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("query")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.query.is_set() {
            if let Some(inner) = self.query.as_option() {
                if let Err(sub) = inner.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(1i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("query")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
        }
        if let ::core::option::Option::Some(__cel_inner) = self.factor.as_ref() {
            let __cel_this = ((*__cel_inner) as f64);
            if !(((!(((__cel_this))).is_nan()) && (!(((__cel_this))).is_infinite()))) {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("factor")), field_type :
                                Some(::protovalidate_buffa::FieldType::Float), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(23), field_name :
                                Some(::std::borrow::Cow::Borrowed("cel")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript :
                                Some(::protovalidate_buffa::Subscript::Index(0u64)), }
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("filter.boost.finite"),
                        message: ::std::borrow::Cow::Borrowed("must be finite"),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for filter::ConstScore {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if !self.query.is_set() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("query")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.query.is_set() {
            if let Some(inner) = self.query.as_option() {
                if let Err(sub) = inner.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(1i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("query")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
        }
        if let ::core::option::Option::Some(__cel_inner) = self.score.as_ref() {
            let __cel_this = ((*__cel_inner) as f64);
            if !(((!(((__cel_this))).is_nan()) && (!(((__cel_this))).is_infinite()))) {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("score")), field_type :
                                Some(::protovalidate_buffa::FieldType::Float), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(23), field_name :
                                Some(::std::borrow::Cow::Borrowed("cel")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript :
                                Some(::protovalidate_buffa::Subscript::Index(0u64)), }
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed(
                            "filter.const_score.finite",
                        ),
                        message: ::std::borrow::Cow::Borrowed("must be finite"),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for __buffa::view::filter::ConstScoreView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if !self.query.is_set() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("query")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.query.is_set() {
            if let Some(inner) = self.query.as_option() {
                if let Err(sub) = inner.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(1i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("query")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
        }
        if let ::core::option::Option::Some(__cel_inner) = self.score.as_ref() {
            let __cel_this = ((*__cel_inner) as f64);
            if !(((!(((__cel_this))).is_nan()) && (!(((__cel_this))).is_infinite()))) {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("score")), field_type :
                                Some(::protovalidate_buffa::FieldType::Float), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(23), field_name :
                                Some(::std::borrow::Cow::Borrowed("cel")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript :
                                Some(::protovalidate_buffa::Subscript::Index(0u64)), }
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed(
                            "filter.const_score.finite",
                        ),
                        message: ::std::borrow::Cow::Borrowed("must be finite"),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for filter::DisjunctionMax {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.queries.len() < 1usize {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("queries")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(18i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("repeated")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("min_items")), field_type :
                            Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("repeated.min_items"),
                    message: ::std::borrow::Cow::Owned(
                        ::std::format!(
                            "list must contain at least {} items (got {})", 1usize, self
                            .queries.len()
                        ),
                    ),
                    for_key: false,
                });
        }
        for (idx, elem) in self.queries.iter().enumerate() {
            if let Err(sub) = elem.validate() {
                violations
                    .extend(
                        sub
                            .violations
                            .into_iter()
                            .map(|mut v| {
                                v.field
                                    .elements
                                    .insert(
                                        0,
                                        ::protovalidate_buffa::FieldPathElement {
                                            field_number: Some(1i32),
                                            field_name: Some(::std::borrow::Cow::Borrowed("queries")),
                                            field_type: Some(::protovalidate_buffa::FieldType::Message),
                                            key_type: None,
                                            value_type: None,
                                            subscript: Some(
                                                ::protovalidate_buffa::Subscript::Index(idx as u64),
                                            ),
                                        },
                                    );
                                v
                            }),
                    );
            }
        }
        if let Some(v) = self.tie_breaker {
            if !(v >= 0f32) {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("tie_breaker")),
                                field_type : Some(::protovalidate_buffa::FieldType::Float),
                                key_type : None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(1i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("float")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("gte")), field_type :
                                Some(::protovalidate_buffa::FieldType::Float), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("float.gte"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let ::core::option::Option::Some(__cel_inner) = self.tie_breaker.as_ref() {
            let __cel_this = ((*__cel_inner) as f64);
            if !(((!(((__cel_this))).is_nan()) && (!(((__cel_this))).is_infinite()))) {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("tie_breaker")),
                                field_type : Some(::protovalidate_buffa::FieldType::Float),
                                key_type : None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(23), field_name :
                                Some(::std::borrow::Cow::Borrowed("cel")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript :
                                Some(::protovalidate_buffa::Subscript::Index(0u64)), }
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed(
                            "filter.disjunction_max.tie_breaker.finite",
                        ),
                        message: ::std::borrow::Cow::Borrowed("must be finite"),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for __buffa::view::filter::DisjunctionMaxView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.queries.len() < 1usize {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("queries")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(18i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("repeated")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("min_items")), field_type :
                            Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("repeated.min_items"),
                    message: ::std::borrow::Cow::Owned(
                        ::std::format!(
                            "list must contain at least {} items (got {})", 1usize, self
                            .queries.len()
                        ),
                    ),
                    for_key: false,
                });
        }
        for (idx, elem) in self.queries.iter().enumerate() {
            if let Err(sub) = elem.validate() {
                violations
                    .extend(
                        sub
                            .violations
                            .into_iter()
                            .map(|mut v| {
                                v.field
                                    .elements
                                    .insert(
                                        0,
                                        ::protovalidate_buffa::FieldPathElement {
                                            field_number: Some(1i32),
                                            field_name: Some(::std::borrow::Cow::Borrowed("queries")),
                                            field_type: Some(::protovalidate_buffa::FieldType::Message),
                                            key_type: None,
                                            value_type: None,
                                            subscript: Some(
                                                ::protovalidate_buffa::Subscript::Index(idx as u64),
                                            ),
                                        },
                                    );
                                v
                            }),
                    );
            }
        }
        if let Some(v) = self.tie_breaker {
            if !(v >= 0f32) {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("tie_breaker")),
                                field_type : Some(::protovalidate_buffa::FieldType::Float),
                                key_type : None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(1i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("float")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("gte")), field_type :
                                Some(::protovalidate_buffa::FieldType::Float), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("float.gte"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let ::core::option::Option::Some(__cel_inner) = self.tie_breaker.as_ref() {
            let __cel_this = ((*__cel_inner) as f64);
            if !(((!(((__cel_this))).is_nan()) && (!(((__cel_this))).is_infinite()))) {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("tie_breaker")),
                                field_type : Some(::protovalidate_buffa::FieldType::Float),
                                key_type : None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(23), field_name :
                                Some(::std::borrow::Cow::Borrowed("cel")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript :
                                Some(::protovalidate_buffa::Subscript::Index(0u64)), }
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed(
                            "filter.disjunction_max.tie_breaker.finite",
                        ),
                        message: ::std::borrow::Cow::Borrowed("must be finite"),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for filter::MoreLikeThis {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        match &self.source {
            Some(__buffa::oneof::filter::more_like_this::Source::Id(v)) => {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("id")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
            Some(__buffa::oneof::filter::more_like_this::Source::Fields(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(2i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("fields")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            None => {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                None, field_name :
                                Some(::std::borrow::Cow::Borrowed("source")), field_type :
                                None, key_type : None, value_type : None, subscript : None,
                                },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath::default(),
                        rule_id: ::std::borrow::Cow::Borrowed("required"),
                        message: ::std::borrow::Cow::Borrowed(
                            "at least one variant is required",
                        ),
                        for_key: false,
                    });
            }
        }
        if let ::core::option::Option::Some(__cel_inner) = self.boost_factor.as_ref() {
            let __cel_this = ((*__cel_inner) as f64);
            if !(((!(((__cel_this))).is_nan()) && (!(((__cel_this))).is_infinite()))) {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(9i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("boost_factor")),
                                field_type : Some(::protovalidate_buffa::FieldType::Float),
                                key_type : None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(23), field_name :
                                Some(::std::borrow::Cow::Borrowed("cel")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript :
                                Some(::protovalidate_buffa::Subscript::Index(0u64)), }
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed(
                            "filter.more_like_this.boost_factor.finite",
                        ),
                        message: ::std::borrow::Cow::Borrowed("must be finite"),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for __buffa::view::filter::MoreLikeThisView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        match &self.source {
            Some(__buffa::view::oneof::filter::more_like_this::Source::Id(v)) => {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("id")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
            Some(__buffa::view::oneof::filter::more_like_this::Source::Fields(v)) => {
                if let Err(sub) = v.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(2i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("fields")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
            None => {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                None, field_name :
                                Some(::std::borrow::Cow::Borrowed("source")), field_type :
                                None, key_type : None, value_type : None, subscript : None,
                                },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath::default(),
                        rule_id: ::std::borrow::Cow::Borrowed("required"),
                        message: ::std::borrow::Cow::Borrowed(
                            "at least one variant is required",
                        ),
                        for_key: false,
                    });
            }
        }
        if let ::core::option::Option::Some(__cel_inner) = self.boost_factor.as_ref() {
            let __cel_this = ((*__cel_inner) as f64);
            if !(((!(((__cel_this))).is_nan()) && (!(((__cel_this))).is_infinite()))) {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(9i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("boost_factor")),
                                field_type : Some(::protovalidate_buffa::FieldType::Float),
                                key_type : None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(23), field_name :
                                Some(::std::borrow::Cow::Borrowed("cel")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript :
                                Some(::protovalidate_buffa::Subscript::Index(0u64)), }
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed(
                            "filter.more_like_this.boost_factor.finite",
                        ),
                        message: ::std::borrow::Cow::Borrowed("must be finite"),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for filter::more_like_this::Fields {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.fields.len() < 1usize {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("fields")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(18i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("repeated")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("min_items")), field_type :
                            Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("repeated.min_items"),
                    message: ::std::borrow::Cow::Owned(
                        ::std::format!(
                            "list must contain at least {} items (got {})", 1usize, self
                            .fields.len()
                        ),
                    ),
                    for_key: false,
                });
        }
        for (idx, elem) in self.fields.iter().enumerate() {
            if let Err(sub) = elem.validate() {
                violations
                    .extend(
                        sub
                            .violations
                            .into_iter()
                            .map(|mut v| {
                                v.field
                                    .elements
                                    .insert(
                                        0,
                                        ::protovalidate_buffa::FieldPathElement {
                                            field_number: Some(1i32),
                                            field_name: Some(::std::borrow::Cow::Borrowed("fields")),
                                            field_type: Some(::protovalidate_buffa::FieldType::Message),
                                            key_type: None,
                                            value_type: None,
                                            subscript: Some(
                                                ::protovalidate_buffa::Subscript::Index(idx as u64),
                                            ),
                                        },
                                    );
                                v
                            }),
                    );
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate
for __buffa::view::filter::more_like_this::FieldsView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.fields.len() < 1usize {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("fields")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(18i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("repeated")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("min_items")), field_type :
                            Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("repeated.min_items"),
                    message: ::std::borrow::Cow::Owned(
                        ::std::format!(
                            "list must contain at least {} items (got {})", 1usize, self
                            .fields.len()
                        ),
                    ),
                    for_key: false,
                });
        }
        for (idx, elem) in self.fields.iter().enumerate() {
            if let Err(sub) = elem.validate() {
                violations
                    .extend(
                        sub
                            .violations
                            .into_iter()
                            .map(|mut v| {
                                v.field
                                    .elements
                                    .insert(
                                        0,
                                        ::protovalidate_buffa::FieldPathElement {
                                            field_number: Some(1i32),
                                            field_name: Some(::std::borrow::Cow::Borrowed("fields")),
                                            field_type: Some(::protovalidate_buffa::FieldType::Message),
                                            key_type: None,
                                            value_type: None,
                                            subscript: Some(
                                                ::protovalidate_buffa::Subscript::Index(idx as u64),
                                            ),
                                        },
                                    );
                                v
                            }),
                    );
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for filter::more_like_this::fields::Field {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.field.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("field")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.field.is_some() {
            if let Some(v) = self.field.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("field")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if !self.values.is_set() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(2i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("values")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.values.is_set() {}
        if let Some(v) = self.collection.as_deref() {
            if v.chars().count() < 1usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(3i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("collection")), field_type
                                : Some(::protovalidate_buffa::FieldType::String), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(14i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate
for __buffa::view::filter::more_like_this::fields::FieldView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.field.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("field")), field_type :
                            Some(::protovalidate_buffa::FieldType::String), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.field.is_some() {
            if let Some(v) = self.field.as_deref() {
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("field")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(14i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if !self.values.is_set() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(2i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("values")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.values.is_set() {}
        if let Some(v) = self.collection.as_deref() {
            if v.chars().count() < 1usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(3i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("collection")), field_type
                                : Some(::protovalidate_buffa::FieldType::String), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(14i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("string")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("min_len")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("string.min_len"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for FindQuery {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if !self.filter.is_set() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("filter")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.filter.is_set() {
            if let Some(inner) = self.filter.as_option() {
                if let Err(sub) = inner.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(1i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("filter")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for __buffa::view::FindQueryView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if !self.filter.is_set() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("filter")), field_type :
                            Some(::protovalidate_buffa::FieldType::Message), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(25i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("required")), field_type :
                            Some(::protovalidate_buffa::FieldType::Bool), key_type :
                            None, value_type : None, subscript : None, },
                        ],
                    },
                    rule_id: ::std::borrow::Cow::Borrowed("required"),
                    message: ::std::borrow::Cow::Borrowed("value is required"),
                    for_key: false,
                });
        }
        if self.filter.is_set() {
            if let Some(inner) = self.filter.as_option() {
                if let Err(sub) = inner.validate() {
                    violations
                        .extend(
                            sub
                                .violations
                                .into_iter()
                                .map(|mut v| {
                                    v.field
                                        .elements
                                        .insert(
                                            0,
                                            ::protovalidate_buffa::FieldPathElement {
                                                field_number: Some(1i32),
                                                field_name: Some(::std::borrow::Cow::Borrowed("filter")),
                                                field_type: Some(::protovalidate_buffa::FieldType::Message),
                                                key_type: None,
                                                value_type: None,
                                                subscript: None,
                                            },
                                        );
                                    v
                                }),
                        );
                }
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for FindResult {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        for (idx, elem) in self.records.iter().enumerate() {
            if let Err(sub) = elem.validate() {
                violations
                    .extend(
                        sub
                            .violations
                            .into_iter()
                            .map(|mut v| {
                                v.field
                                    .elements
                                    .insert(
                                        0,
                                        ::protovalidate_buffa::FieldPathElement {
                                            field_number: Some(1i32),
                                            field_name: Some(::std::borrow::Cow::Borrowed("records")),
                                            field_type: Some(::protovalidate_buffa::FieldType::Message),
                                            key_type: None,
                                            value_type: None,
                                            subscript: Some(
                                                ::protovalidate_buffa::Subscript::Index(idx as u64),
                                            ),
                                        },
                                    );
                                v
                            }),
                    );
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
#[allow(
    clippy::all,
    unused_mut,
    unused_variables,
    unused_parens,
    dead_code,
    unreachable_patterns,
    reason = "protovalidate-buffa generated validators — codegen emits uniform scaffolding regardless of which rules apply"
)]
impl ::protovalidate_buffa::Validate for __buffa::view::FindResultView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        for (idx, elem) in self.records.iter().enumerate() {
            if let Err(sub) = elem.validate() {
                violations
                    .extend(
                        sub
                            .violations
                            .into_iter()
                            .map(|mut v| {
                                v.field
                                    .elements
                                    .insert(
                                        0,
                                        ::protovalidate_buffa::FieldPathElement {
                                            field_number: Some(1i32),
                                            field_name: Some(::std::borrow::Cow::Borrowed("records")),
                                            field_type: Some(::protovalidate_buffa::FieldType::Message),
                                            key_type: None,
                                            value_type: None,
                                            subscript: Some(
                                                ::protovalidate_buffa::Subscript::Index(idx as u64),
                                            ),
                                        },
                                    );
                                v
                            }),
                    );
            }
        }
        if let Some(index) = violations
            .iter()
            .position(|v| v.rule_id == "__cel_runtime_error__")
        {
            let v = violations.swap_remove(index);
            return ::core::result::Result::Err(::protovalidate_buffa::ValidationError {
                runtime_error: ::core::option::Option::Some(v.message.into_owned()),
                ..::core::default::Default::default()
            });
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(::protovalidate_buffa::ValidationError {
                violations,
                ..::core::default::Default::default()
            })
        }
    }
}
