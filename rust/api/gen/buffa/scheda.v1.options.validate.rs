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
impl ::protovalidate_buffa::Validate for OpenOptions {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if let Some(inner) = self.encryption.as_option() {
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
                                            field_name: Some(
                                                ::std::borrow::Cow::Borrowed("encryption"),
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
        if let Some(inner) = self.author.as_option() {
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
                                            field_number: Some(4i32),
                                            field_name: Some(::std::borrow::Cow::Borrowed("author")),
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
        if let Some(v) = self.writer.as_deref() {
            if (::core::convert::AsRef::<str>::as_ref(&(v))).chars().count() < 1usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("writer")), field_type :
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
            if (::core::convert::AsRef::<str>::as_ref(&(v))).chars().count() > 32usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("writer")), field_type :
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
                                Some(3i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("max_len")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("string.max_len"),
                        message: ::std::borrow::Cow::Owned(
                            ::std::format!(
                                "value length must be at most {} characters", 32usize
                            ),
                        ),
                        for_key: false,
                    });
            }
            {
                static RE_WRITER: ::std::sync::OnceLock<
                    ::protovalidate_buffa::regex::Regex,
                > = ::std::sync::OnceLock::new();
                let re = RE_WRITER
                    .get_or_init(|| {
                        ::protovalidate_buffa::regex::Regex::new(
                                "^[a-z0-9]([a-z0-9-]*[a-z0-9])?$",
                            )
                            .expect("pattern regex compiled at code-gen time")
                    });
                if !re.is_match(::core::convert::AsRef::<str>::as_ref(&(v))) {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(5i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("writer")), field_type :
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
                                    Some(6i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("pattern")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.pattern"),
                            message: ::std::borrow::Cow::Owned(
                                ::std::format!(
                                    "value must match pattern /{}/",
                                    "^[a-z0-9]([a-z0-9-]*[a-z0-9])?$"
                                ),
                            ),
                            for_key: false,
                        });
                }
            }
        }
        if let Some(v) = self.index_poll_ms {
            if v < 10u32 {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(6i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("index_poll_ms")),
                                field_type : Some(::protovalidate_buffa::FieldType::Uint32),
                                key_type : None, value_type : None, subscript : None, },
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
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("gte")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint32), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("uint32.gte"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
            if v > 3600000u32 {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(6i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("index_poll_ms")),
                                field_type : Some(::protovalidate_buffa::FieldType::Uint32),
                                key_type : None, value_type : None, subscript : None, },
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
        if let Some(v) = self.mode {
            #[allow(unused_imports, reason = "only closed enums need the trait")]
            use ::buffa::Enumeration as _;
            {
                let raw_val: i32 = v.to_i32();
                if <scheda::v1::OpenMode as ::buffa::Enumeration>::from_i32(raw_val)
                    .is_none()
                {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(7i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("mode")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Enum), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(16i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("enum")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("defined_only")),
                                    field_type : Some(::protovalidate_buffa::FieldType::Bool),
                                    key_type : None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("enum.defined_only"),
                            message: ::std::borrow::Cow::Owned(
                                ::std::format!(
                                    "value {} is not a defined enum value", raw_val
                                ),
                            ),
                            for_key: false,
                        });
                }
            }
        }
        if let Some(v) = self.write_timeout_ms {
            if v < 1u32 {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(8i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("write_timeout_ms")),
                                field_type : Some(::protovalidate_buffa::FieldType::Uint32),
                                key_type : None, value_type : None, subscript : None, },
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
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("gte")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint32), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("uint32.gte"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(inner) = self.signing.as_option() {
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
                                            field_number: Some(9i32),
                                            field_name: Some(::std::borrow::Cow::Borrowed("signing")),
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
        if let Some(v) = self.index_wait_timeout_ms {
            if v < 1u32 {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(10i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("index_wait_timeout_ms")),
                                field_type : Some(::protovalidate_buffa::FieldType::Uint32),
                                key_type : None, value_type : None, subscript : None, },
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
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("gte")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint32), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("uint32.gte"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(v) = self.filter_max_depth {
            if v < 1u32 {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(11i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("filter_max_depth")),
                                field_type : Some(::protovalidate_buffa::FieldType::Uint32),
                                key_type : None, value_type : None, subscript : None, },
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
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("gte")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint32), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("uint32.gte"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
            if v > 100u32 {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(11i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("filter_max_depth")),
                                field_type : Some(::protovalidate_buffa::FieldType::Uint32),
                                key_type : None, value_type : None, subscript : None, },
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
        if let Some(v) = self.work_tree {
            #[allow(unused_imports, reason = "only closed enums need the trait")]
            use ::buffa::Enumeration as _;
            {
                let raw_val: i32 = v.to_i32();
                if <scheda::v1::WorkTree as ::buffa::Enumeration>::from_i32(raw_val)
                    .is_none()
                {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(12i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("work_tree")), field_type
                                    : Some(::protovalidate_buffa::FieldType::Enum), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(16i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("enum")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("defined_only")),
                                    field_type : Some(::protovalidate_buffa::FieldType::Bool),
                                    key_type : None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("enum.defined_only"),
                            message: ::std::borrow::Cow::Owned(
                                ::std::format!(
                                    "value {} is not a defined enum value", raw_val
                                ),
                            ),
                            for_key: false,
                        });
                }
            }
        }
        if let ::core::option::Option::Some(__cel_inner) = self.mirror.as_ref() {
            let __cel_this = ::core::convert::AsRef::<str>::as_ref(__cel_inner);
            if !((::protovalidate_buffa::rules::string::is_uri(((__cel_this)))
                || (((((__cel_this))).contains(("@"))
                    && (((__cel_this))).contains((":")))
                    && (!(((__cel_this))).contains(("://"))))))
            {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("mirror")), field_type :
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
                            "open_options.mirror.form",
                        ),
                        message: ::std::borrow::Cow::Borrowed(
                            "must be an absolute URI (https://…, ssh://…, file:///…) or an scp-style address (git@host:path)",
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
impl ::protovalidate_buffa::Validate for __buffa::view::OpenOptionsView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if let Some(inner) = self.encryption.as_option() {
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
                                            field_name: Some(
                                                ::std::borrow::Cow::Borrowed("encryption"),
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
        if let Some(inner) = self.author.as_option() {
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
                                            field_number: Some(4i32),
                                            field_name: Some(::std::borrow::Cow::Borrowed("author")),
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
        if let Some(v) = self.writer.as_deref() {
            if (::core::convert::AsRef::<str>::as_ref(&(v))).chars().count() < 1usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("writer")), field_type :
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
            if (::core::convert::AsRef::<str>::as_ref(&(v))).chars().count() > 32usize {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("writer")), field_type :
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
                                Some(3i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("max_len")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint64), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("string.max_len"),
                        message: ::std::borrow::Cow::Owned(
                            ::std::format!(
                                "value length must be at most {} characters", 32usize
                            ),
                        ),
                        for_key: false,
                    });
            }
            {
                static RE_WRITER: ::std::sync::OnceLock<
                    ::protovalidate_buffa::regex::Regex,
                > = ::std::sync::OnceLock::new();
                let re = RE_WRITER
                    .get_or_init(|| {
                        ::protovalidate_buffa::regex::Regex::new(
                                "^[a-z0-9]([a-z0-9-]*[a-z0-9])?$",
                            )
                            .expect("pattern regex compiled at code-gen time")
                    });
                if !re.is_match(::core::convert::AsRef::<str>::as_ref(&(v))) {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(5i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("writer")), field_type :
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
                                    Some(6i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("pattern")), field_type :
                                    Some(::protovalidate_buffa::FieldType::String), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.pattern"),
                            message: ::std::borrow::Cow::Owned(
                                ::std::format!(
                                    "value must match pattern /{}/",
                                    "^[a-z0-9]([a-z0-9-]*[a-z0-9])?$"
                                ),
                            ),
                            for_key: false,
                        });
                }
            }
        }
        if let Some(v) = self.index_poll_ms {
            if v < 10u32 {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(6i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("index_poll_ms")),
                                field_type : Some(::protovalidate_buffa::FieldType::Uint32),
                                key_type : None, value_type : None, subscript : None, },
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
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("gte")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint32), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("uint32.gte"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
            if v > 3600000u32 {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(6i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("index_poll_ms")),
                                field_type : Some(::protovalidate_buffa::FieldType::Uint32),
                                key_type : None, value_type : None, subscript : None, },
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
        if let Some(v) = self.mode {
            #[allow(unused_imports, reason = "only closed enums need the trait")]
            use ::buffa::Enumeration as _;
            {
                let raw_val: i32 = v.to_i32();
                if <scheda::v1::OpenMode as ::buffa::Enumeration>::from_i32(raw_val)
                    .is_none()
                {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(7i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("mode")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Enum), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(16i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("enum")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("defined_only")),
                                    field_type : Some(::protovalidate_buffa::FieldType::Bool),
                                    key_type : None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("enum.defined_only"),
                            message: ::std::borrow::Cow::Owned(
                                ::std::format!(
                                    "value {} is not a defined enum value", raw_val
                                ),
                            ),
                            for_key: false,
                        });
                }
            }
        }
        if let Some(v) = self.write_timeout_ms {
            if v < 1u32 {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(8i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("write_timeout_ms")),
                                field_type : Some(::protovalidate_buffa::FieldType::Uint32),
                                key_type : None, value_type : None, subscript : None, },
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
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("gte")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint32), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("uint32.gte"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(inner) = self.signing.as_option() {
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
                                            field_number: Some(9i32),
                                            field_name: Some(::std::borrow::Cow::Borrowed("signing")),
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
        if let Some(v) = self.index_wait_timeout_ms {
            if v < 1u32 {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(10i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("index_wait_timeout_ms")),
                                field_type : Some(::protovalidate_buffa::FieldType::Uint32),
                                key_type : None, value_type : None, subscript : None, },
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
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("gte")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint32), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("uint32.gte"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
        }
        if let Some(v) = self.filter_max_depth {
            if v < 1u32 {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(11i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("filter_max_depth")),
                                field_type : Some(::protovalidate_buffa::FieldType::Uint32),
                                key_type : None, value_type : None, subscript : None, },
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
                                Some(5i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("gte")), field_type :
                                Some(::protovalidate_buffa::FieldType::Uint32), key_type :
                                None, value_type : None, subscript : None, },
                            ],
                        },
                        rule_id: ::std::borrow::Cow::Borrowed("uint32.gte"),
                        message: ::std::borrow::Cow::Borrowed(""),
                        for_key: false,
                    });
            }
            if v > 100u32 {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(11i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("filter_max_depth")),
                                field_type : Some(::protovalidate_buffa::FieldType::Uint32),
                                key_type : None, value_type : None, subscript : None, },
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
        if let Some(v) = self.work_tree {
            #[allow(unused_imports, reason = "only closed enums need the trait")]
            use ::buffa::Enumeration as _;
            {
                let raw_val: i32 = v.to_i32();
                if <scheda::v1::WorkTree as ::buffa::Enumeration>::from_i32(raw_val)
                    .is_none()
                {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(12i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("work_tree")), field_type
                                    : Some(::protovalidate_buffa::FieldType::Enum), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(16i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("enum")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("defined_only")),
                                    field_type : Some(::protovalidate_buffa::FieldType::Bool),
                                    key_type : None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("enum.defined_only"),
                            message: ::std::borrow::Cow::Owned(
                                ::std::format!(
                                    "value {} is not a defined enum value", raw_val
                                ),
                            ),
                            for_key: false,
                        });
                }
            }
        }
        if let ::core::option::Option::Some(__cel_inner) = self.mirror.as_ref() {
            let __cel_this = ::core::convert::AsRef::<str>::as_ref(__cel_inner);
            if !((::protovalidate_buffa::rules::string::is_uri(((__cel_this)))
                || (((((__cel_this))).contains(("@"))
                    && (((__cel_this))).contains((":")))
                    && (!(((__cel_this))).contains(("://"))))))
            {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("mirror")), field_type :
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
                            "open_options.mirror.form",
                        ),
                        message: ::std::borrow::Cow::Borrowed(
                            "must be an absolute URI (https://…, ssh://…, file:///…) or an scp-style address (git@host:path)",
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
impl ::protovalidate_buffa::Validate for Signing {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        match &self.key {
            Some(__buffa::oneof::signing::Key::SshKey(v)) => {
                if (::core::convert::AsRef::<str>::as_ref(&(v))).chars().count() < 1usize
                {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("ssh_key")), field_type :
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
            Some(__buffa::oneof::signing::Key::OpenpgpKey(v)) => {
                if (::core::convert::AsRef::<str>::as_ref(&(v))).chars().count() < 1usize
                {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("openpgp_key")),
                                    field_type : Some(::protovalidate_buffa::FieldType::String),
                                    key_type : None, value_type : None, subscript : None, },
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
            None => {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                None, field_name :
                                Some(::std::borrow::Cow::Borrowed("key")), field_type :
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
impl ::protovalidate_buffa::Validate for __buffa::view::SigningView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        match &self.key {
            Some(__buffa::view::oneof::signing::Key::SshKey(v)) => {
                if (::core::convert::AsRef::<str>::as_ref(&(v))).chars().count() < 1usize
                {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("ssh_key")), field_type :
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
            Some(__buffa::view::oneof::signing::Key::OpenpgpKey(v)) => {
                if (::core::convert::AsRef::<str>::as_ref(&(v))).chars().count() < 1usize
                {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("openpgp_key")),
                                    field_type : Some(::protovalidate_buffa::FieldType::String),
                                    key_type : None, value_type : None, subscript : None, },
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
            None => {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                None, field_name :
                                Some(::std::borrow::Cow::Borrowed("key")), field_type :
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
impl ::protovalidate_buffa::Validate for Author {
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
                {
                    static RE_NAME: ::std::sync::OnceLock<
                        ::protovalidate_buffa::regex::Regex,
                    > = ::std::sync::OnceLock::new();
                    let re = RE_NAME
                        .get_or_init(|| {
                            ::protovalidate_buffa::regex::Regex::new("^[^<>\\n]+$")
                                .expect("pattern regex compiled at code-gen time")
                        });
                    if !re.is_match(::core::convert::AsRef::<str>::as_ref(&(v))) {
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
                                        Some(6i32), field_name :
                                        Some(::std::borrow::Cow::Borrowed("pattern")), field_type :
                                        Some(::protovalidate_buffa::FieldType::String), key_type :
                                        None, value_type : None, subscript : None, },
                                    ],
                                },
                                rule_id: ::std::borrow::Cow::Borrowed("string.pattern"),
                                message: ::std::borrow::Cow::Owned(
                                    ::std::format!(
                                        "value must match pattern /{}/", "^[^<>\\n]+$"
                                    ),
                                ),
                                for_key: false,
                            });
                    }
                }
            }
        }
        if self.email.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(2i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("email")), field_type :
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
        if self.email.is_some() {
            if let Some(v) = self.email.as_deref() {
                if (::core::convert::AsRef::<str>::as_ref(&(v))).is_empty() {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("email")), field_type :
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
                                    Some(12i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("email")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Bool), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.email_empty"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                } else if !::protovalidate_buffa::rules::string::is_email(
                    ::core::convert::AsRef::<str>::as_ref(&(v)),
                ) {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("email")), field_type :
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
                                    Some(12i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("email")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Bool), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.email"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if let ::core::option::Option::Some(__cel_inner) = self.email.as_ref() {
            let __cel_this = ::core::convert::AsRef::<str>::as_ref(__cel_inner);
            if !(::protovalidate_buffa::rules::string::is_email(((__cel_this)))) {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("email")), field_type :
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
                        rule_id: ::std::borrow::Cow::Borrowed("author.email.format"),
                        message: ::std::borrow::Cow::Borrowed(
                            "must be a valid email address",
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
impl ::protovalidate_buffa::Validate for __buffa::view::AuthorView<'_> {
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
                {
                    static RE_NAME: ::std::sync::OnceLock<
                        ::protovalidate_buffa::regex::Regex,
                    > = ::std::sync::OnceLock::new();
                    let re = RE_NAME
                        .get_or_init(|| {
                            ::protovalidate_buffa::regex::Regex::new("^[^<>\\n]+$")
                                .expect("pattern regex compiled at code-gen time")
                        });
                    if !re.is_match(::core::convert::AsRef::<str>::as_ref(&(v))) {
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
                                        Some(6i32), field_name :
                                        Some(::std::borrow::Cow::Borrowed("pattern")), field_type :
                                        Some(::protovalidate_buffa::FieldType::String), key_type :
                                        None, value_type : None, subscript : None, },
                                    ],
                                },
                                rule_id: ::std::borrow::Cow::Borrowed("string.pattern"),
                                message: ::std::borrow::Cow::Owned(
                                    ::std::format!(
                                        "value must match pattern /{}/", "^[^<>\\n]+$"
                                    ),
                                ),
                                for_key: false,
                            });
                    }
                }
            }
        }
        if self.email.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(2i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("email")), field_type :
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
        if self.email.is_some() {
            if let Some(v) = self.email.as_deref() {
                if (::core::convert::AsRef::<str>::as_ref(&(v))).is_empty() {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("email")), field_type :
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
                                    Some(12i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("email")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Bool), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.email_empty"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                } else if !::protovalidate_buffa::rules::string::is_email(
                    ::core::convert::AsRef::<str>::as_ref(&(v)),
                ) {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("email")), field_type :
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
                                    Some(12i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("email")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Bool), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("string.email"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
                }
            }
        }
        if let ::core::option::Option::Some(__cel_inner) = self.email.as_ref() {
            let __cel_this = ::core::convert::AsRef::<str>::as_ref(__cel_inner);
            if !(::protovalidate_buffa::rules::string::is_email(((__cel_this)))) {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                Some(2i32), field_name :
                                Some(::std::borrow::Cow::Borrowed("email")), field_type :
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
                        rule_id: ::std::borrow::Cow::Borrowed("author.email.format"),
                        message: ::std::borrow::Cow::Borrowed(
                            "must be a valid email address",
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
impl ::protovalidate_buffa::Validate for Encryption {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        match &self.secret {
            Some(__buffa::oneof::encryption::Secret::Passphrase(v)) => {
                if (::core::convert::AsRef::<str>::as_ref(&(v))).chars().count() < 1usize
                {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("passphrase")), field_type
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
                            message: ::std::borrow::Cow::Owned(
                                ::std::format!(
                                    "value length must be at least {} characters", 1usize
                                ),
                            ),
                            for_key: false,
                        });
                }
            }
            Some(__buffa::oneof::encryption::Secret::Identity(v)) => {
                if (::core::convert::AsRef::<str>::as_ref(&(v))).chars().count() < 1usize
                {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("identity")), field_type :
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
            None => {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                None, field_name :
                                Some(::std::borrow::Cow::Borrowed("secret")), field_type :
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
impl ::protovalidate_buffa::Validate for __buffa::view::EncryptionView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        match &self.secret {
            Some(__buffa::view::oneof::encryption::Secret::Passphrase(v)) => {
                if (::core::convert::AsRef::<str>::as_ref(&(v))).chars().count() < 1usize
                {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("passphrase")), field_type
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
                            message: ::std::borrow::Cow::Owned(
                                ::std::format!(
                                    "value length must be at least {} characters", 1usize
                                ),
                            ),
                            for_key: false,
                        });
                }
            }
            Some(__buffa::view::oneof::encryption::Secret::Identity(v)) => {
                if (::core::convert::AsRef::<str>::as_ref(&(v))).chars().count() < 1usize
                {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(2i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("identity")), field_type :
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
            None => {
                violations
                    .push(::protovalidate_buffa::Violation {
                        field: ::protovalidate_buffa::FieldPath {
                            elements: ::std::vec![
                                ::protovalidate_buffa::FieldPathElement { field_number :
                                None, field_name :
                                Some(::std::borrow::Cow::Borrowed("secret")), field_type :
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
