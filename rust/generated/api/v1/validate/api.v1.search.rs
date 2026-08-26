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
impl ::protovalidate_buffa::Validate for SearchQuery {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.text.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("text")), field_type :
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
        if self.text.is_some() {
            if let Some(v) = self.text.as_ref() {
                let v: ::std::string::String = v.clone();
                if v.chars().count() < 1usize {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("text")), field_type :
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
                                            field_number: Some(3i32),
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
        if let Some(v) = self.lexical_weight {
            if !(v >= 0f64) {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(4i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("lexical_weight")),
                                field_type : Some(::protovalidate_buffa::FieldType::Double),
                                key_type : None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("double")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("gte")), field_type :
                                Some(::protovalidate_buffa::FieldType::Double), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("double.gte"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(v) = self.vector_weight {
            if !(v >= 0f64) {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("vector_weight")),
                                field_type : Some(::protovalidate_buffa::FieldType::Double),
                                key_type : None, value_type : None, subscript : None, },
                            ],
                        },
                        rule: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("double")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
                                None, value_type : None, subscript : None, },
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("gte")), field_type :
                                Some(::protovalidate_buffa::FieldType::Double), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("double.gte"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let ::core::option::Option::Some(__cel_inner) = self.lexical_weight.as_ref() {
            let __cel_this = ((*__cel_inner) as f64);
            if !(((!(((__cel_this))).is_nan()) && (!(((__cel_this))).is_infinite()))) {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(4i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("lexical_weight")),
                                field_type : Some(::protovalidate_buffa::FieldType::Double),
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
                            "search.lexical_weight.finite",
                        ),
                        message: ::std::borrow::Cow::Borrowed("must be finite"),
                        for_key: false,
                    });
            }
        }
        if let ::core::option::Option::Some(__cel_inner) = self.vector_weight.as_ref() {
            let __cel_this = ((*__cel_inner) as f64);
            if !(((!(((__cel_this))).is_nan()) && (!(((__cel_this))).is_infinite()))) {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("vector_weight")),
                                field_type : Some(::protovalidate_buffa::FieldType::Double),
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
                            "search.vector_weight.finite",
                        ),
                        message: ::std::borrow::Cow::Borrowed("must be finite"),
                        for_key: false,
                    });
            }
        }
        let (
            rt_violation,
            violations,
        ): (
            ::std::option::Option<::protovalidate_buffa::Violation>,
            ::std::vec::Vec<::protovalidate_buffa::Violation>,
        ) = {
            let mut rt = None;
            let mut rest = ::std::vec::Vec::with_capacity(violations.len());
            for v in violations {
                if rt.is_none() && v.rule_id == "__cel_runtime_error__" {
                    rt = Some(v);
                } else {
                    rest.push(v);
                }
            }
            (rt, rest)
        };
        if let Some(v) = rt_violation {
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
impl ::protovalidate_buffa::Validate for SearchHits {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        for (idx, elem) in self.hits.iter().enumerate() {
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
                                            field_name: Some(::std::borrow::Cow::Borrowed("hits")),
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
        let (
            rt_violation,
            violations,
        ): (
            ::std::option::Option<::protovalidate_buffa::Violation>,
            ::std::vec::Vec<::protovalidate_buffa::Violation>,
        ) = {
            let mut rt = None;
            let mut rest = ::std::vec::Vec::with_capacity(violations.len());
            for v in violations {
                if rt.is_none() && v.rule_id == "__cel_runtime_error__" {
                    rt = Some(v);
                } else {
                    rest.push(v);
                }
            }
            (rt, rest)
        };
        if let Some(v) = rt_violation {
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
impl ::protovalidate_buffa::Validate for search_hits::Hit {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        let (
            rt_violation,
            violations,
        ): (
            ::std::option::Option<::protovalidate_buffa::Violation>,
            ::std::vec::Vec<::protovalidate_buffa::Violation>,
        ) = {
            let mut rt = None;
            let mut rest = ::std::vec::Vec::with_capacity(violations.len());
            for v in violations {
                if rt.is_none() && v.rule_id == "__cel_runtime_error__" {
                    rt = Some(v);
                } else {
                    rest.push(v);
                }
            }
            (rt, rest)
        };
        if let Some(v) = rt_violation {
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
