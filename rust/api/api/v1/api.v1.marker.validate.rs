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
impl ::protovalidate_buffa::Validate for VaultMarker {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.version.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("version")), field_type :
                            Some(::protovalidate_buffa::FieldType::Int32), key_type :
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
        if self.version.is_some() {
            if let Some(v) = self.version {
                if v != 1i32 {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("version")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Int32), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(3i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("int32")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("const")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Int32), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("int32.const"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
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
impl ::protovalidate_buffa::Validate for __buffa::view::VaultMarkerView<'_> {
    fn validate(
        &self,
    ) -> ::core::result::Result<(), ::protovalidate_buffa::ValidationError> {
        let mut violations: ::std::vec::Vec<::protovalidate_buffa::Violation> = ::std::vec::Vec::new();
        if self.version.is_none() {
            violations
                .push(::protovalidate_buffa::Violation {
                    field: ::protovalidate_buffa::FieldPath {
                        elements: ::std::vec![
                            ::protovalidate_buffa::FieldPathElement { field_number :
                            Some(1i32), field_name :
                            Some(::std::borrow::Cow::Borrowed("version")), field_type :
                            Some(::protovalidate_buffa::FieldType::Int32), key_type :
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
        if self.version.is_some() {
            if let Some(v) = self.version {
                if v != 1i32 {
                    violations
                        .push(::protovalidate_buffa::Violation {
                            field: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("version")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Int32), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule: ::protovalidate_buffa::FieldPath {
                                elements: ::std::vec![
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(3i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("int32")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Message), key_type :
                                    None, value_type : None, subscript : None, },
                                    ::protovalidate_buffa::FieldPathElement { field_number :
                                    Some(1i32), field_name :
                                    Some(::std::borrow::Cow::Borrowed("const")), field_type :
                                    Some(::protovalidate_buffa::FieldType::Int32), key_type :
                                    None, value_type : None, subscript : None, },
                                ],
                            },
                            rule_id: ::std::borrow::Cow::Borrowed("int32.const"),
                            message: ::std::borrow::Cow::Borrowed(""),
                            for_key: false,
                        });
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
