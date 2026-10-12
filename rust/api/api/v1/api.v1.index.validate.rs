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
impl ::protovalidate_buffa::Validate for ReindexResult {
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
impl ::protovalidate_buffa::Validate for __buffa::view::ReindexResultView<'_> {
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
impl ::protovalidate_buffa::Validate for IndexColumn {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.name.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("name")), field_type :
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
        if self.name.is_some() {
            if let Some(v) = self.name.as_deref() {
                if (::core::convert::AsRef::<str>::as_ref(&(v))).chars().count() < 1usize
                {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("name")), field_type :
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
                            message: ::std::borrow::Cow::Owned(
                                ::std::format!(
                                    "value length must be at least {} characters", 1usize
                                ),
                            ),
                            for_key: false,
                        });
                }
            }
        }
        if self.kind.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(2i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("kind")), field_type :
                            Some(::protovalidate_buffa::FieldType::Enum), key_type :
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
        if self.kind.is_some() {}
        if let ::core::option::Option::Some(__cel_inner) = self.name.as_ref() {
            let __cel_this = ::core::convert::AsRef::<str>::as_ref(__cel_inner);
            if !(((((__cel_this)) != ("id")) && (((__cel_this)) != ("collection")))) {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(1i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("name")), field_type :
                                Some(::protovalidate_buffa::FieldType::String), key_type :
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
                            "index_column.name.not_identity",
                        ),
                        message: ::std::borrow::Cow::Borrowed(
                            "name must not be id or collection",
                        ),
                        for_key: false,
                    });
            }
        }
        if let ::core::option::Option::Some(__cel_inner) = self.kind.as_ref() {
            let __cel_this = ({
                use ::buffa::Enumeration as _;
                (*__cel_inner).to_i32() as i64
            });
            if !(((__cel_this) != (0i64 as i64))) {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("kind")), field_type :
                                Some(::protovalidate_buffa::FieldType::Enum), key_type :
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
                            "index_column.kind.not_unspecified",
                        ),
                        message: ::std::borrow::Cow::Borrowed(
                            "kind must not be unspecified",
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
impl ::protovalidate_buffa::Validate for __buffa::view::IndexColumnView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.name.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("name")), field_type :
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
        if self.name.is_some() {
            if let Some(v) = self.name.as_deref() {
                if (::core::convert::AsRef::<str>::as_ref(&(v))).chars().count() < 1usize
                {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("name")), field_type :
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
                            message: ::std::borrow::Cow::Owned(
                                ::std::format!(
                                    "value length must be at least {} characters", 1usize
                                ),
                            ),
                            for_key: false,
                        });
                }
            }
        }
        if self.kind.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(2i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("kind")), field_type :
                            Some(::protovalidate_buffa::FieldType::Enum), key_type :
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
        if self.kind.is_some() {}
        if let ::core::option::Option::Some(__cel_inner) = self.name.as_ref() {
            let __cel_this = ::core::convert::AsRef::<str>::as_ref(__cel_inner);
            if !(((((__cel_this)) != ("id")) && (((__cel_this)) != ("collection")))) {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(1i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("name")), field_type :
                                Some(::protovalidate_buffa::FieldType::String), key_type :
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
                            "index_column.name.not_identity",
                        ),
                        message: ::std::borrow::Cow::Borrowed(
                            "name must not be id or collection",
                        ),
                        for_key: false,
                    });
            }
        }
        if let ::core::option::Option::Some(__cel_inner) = self.kind.as_ref() {
            let __cel_this = ({
                use ::buffa::Enumeration as _;
                (*__cel_inner).to_i32() as i64
            });
            if !(((__cel_this) != (0i64 as i64))) {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("kind")), field_type :
                                Some(::protovalidate_buffa::FieldType::Enum), key_type :
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
                            "index_column.kind.not_unspecified",
                        ),
                        message: ::std::borrow::Cow::Borrowed(
                            "kind must not be unspecified",
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
impl ::protovalidate_buffa::Validate for IndexSchema {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        for (idx, elem) in self.columns.iter().enumerate() {
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
                                            field_name: Some(::std::borrow::Cow::Borrowed("columns")),
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
        {
            let __cel_this = (&self.columns);
            if !(((((__cel_this)).iter())
                .all(|__cel_iter_c| {
                    (((({
                        let __cel_filtered: ::std::vec::Vec<_> = (((__cel_this)).iter())
                            .filter(|__cel_iter_other| {
                                ((((__cel_iter_other.name.as_deref().unwrap_or("")))
                                    == ((__cel_iter_c.name.as_deref().unwrap_or("")))))
                            })
                            .cloned()
                            .collect();
                        __cel_filtered
                    }))
                        .len() as i64) == (1i64 as i64))
                })))
            {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(1i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("columns")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
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
                            "index_schema.columns.unique_names",
                        ),
                        message: ::std::borrow::Cow::Borrowed(
                            "column names must be unique",
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
impl ::protovalidate_buffa::Validate for __buffa::view::IndexSchemaView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        for (idx, elem) in self.columns.iter().enumerate() {
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
                                            field_name: Some(::std::borrow::Cow::Borrowed("columns")),
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
        {
            let __cel_this = (&self.columns);
            if !(((((__cel_this)).iter())
                .all(|__cel_iter_c| {
                    (((({
                        let __cel_filtered: ::std::vec::Vec<_> = (((__cel_this)).iter())
                            .filter(|__cel_iter_other| {
                                ((((__cel_iter_other.name.as_deref().unwrap_or("")))
                                    == ((__cel_iter_c.name.as_deref().unwrap_or("")))))
                            })
                            .cloned()
                            .collect();
                        __cel_filtered
                    }))
                        .len() as i64) == (1i64 as i64))
                })))
            {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(1i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("columns")), field_type :
                                Some(::protovalidate_buffa::FieldType::Message), key_type :
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
                            "index_schema.columns.unique_names",
                        ),
                        message: ::std::borrow::Cow::Borrowed(
                            "column names must be unique",
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
