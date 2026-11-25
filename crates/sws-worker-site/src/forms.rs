//! Schema-driven form validation engine for `sws-worker-site`.
//!
//! Pure, host-testable validation logic with no Workers runtime types.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sws_security::validate::{
    FieldErrors, Fields, HONEYPOT_FIELD, ValidationError, get_trimmed, is_valid_date,
    is_valid_email, is_valid_phone, is_valid_route, validate_email_field, validate_text_field,
    validate_token_field,
};

use crate::routes::is_valid_form_id;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormSchema {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub turnstile_action: String,
    pub fields: Vec<FormFieldRule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormFieldRule {
    pub name: String,
    #[serde(default)]
    pub required: bool,
    pub kind: FieldKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text {
        max_chars: usize,
        #[serde(default)]
        multiline: bool,
    },
    Email,
    Phone {
        max_chars: usize,
    },
    Date,
    Integer {
        min: u32,
        max: u32,
    },
    Enum(Vec<String>),
    Route {
        #[serde(default)]
        allowed: Option<Vec<String>>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedSubmission {
    pub form_id: String,
    pub source_route: String,
    pub values: BTreeMap<String, Value>,
    pub turnstile_token: Option<String>,
}

pub fn validate_submission(
    schema: &FormSchema,
    fields: &Fields,
) -> Result<ValidatedSubmission, ValidationError> {
    if get_trimmed(fields, HONEYPOT_FIELD).is_some() {
        return Err(ValidationError::Honeypot);
    }

    let mut errs = FieldErrors::new();
    let mut values = BTreeMap::new();
    let mut source_route: Option<String> = None;

    for rule in &schema.fields {
        match &rule.kind {
            FieldKind::Text {
                max_chars,
                multiline,
            } => {
                if let Some(val) = validate_text_field(
                    fields,
                    &rule.name,
                    *max_chars,
                    rule.required,
                    *multiline,
                    &mut errs,
                ) {
                    values.insert(rule.name.clone(), Value::String(val));
                }
            }
            FieldKind::Email => {
                if rule.name == "email" && rule.required {
                    if let Some(email) = validate_email_field(fields, &mut errs) {
                        values.insert(rule.name.clone(), Value::String(email));
                    }
                } else {
                    match get_trimmed(fields, &rule.name) {
                        None => {
                            if rule.required {
                                errs.push((rule.name.clone(), "required"));
                            }
                        }
                        Some(e) if !is_valid_email(e) => {
                            errs.push((rule.name.clone(), "invalid"));
                        }
                        Some(e) => {
                            values.insert(
                                rule.name.clone(),
                                Value::String(e.to_ascii_lowercase()),
                            );
                        }
                    }
                }
            }
            FieldKind::Phone { max_chars } => {
                if let Some(phone) = validate_text_field(
                    fields,
                    &rule.name,
                    *max_chars,
                    rule.required,
                    false,
                    &mut errs,
                ) {
                    if !is_valid_phone(&phone) {
                        errs.push((rule.name.clone(), "invalid"));
                    } else {
                        values.insert(rule.name.clone(), Value::String(phone));
                    }
                }
            }
            FieldKind::Date => {
                if let Some(date) =
                    validate_text_field(fields, &rule.name, 10, rule.required, false, &mut errs)
                {
                    if !is_valid_date(&date) {
                        errs.push((rule.name.clone(), "invalid"));
                    } else {
                        values.insert(rule.name.clone(), Value::String(date));
                    }
                }
            }
            FieldKind::Integer { min, max } => match get_trimmed(fields, &rule.name) {
                None => {
                    if rule.required {
                        errs.push((rule.name.clone(), "required"));
                    }
                }
                Some(raw) => match raw.parse::<u32>() {
                    Ok(n) if (*min..=*max).contains(&n) => {
                        values.insert(rule.name.clone(), Value::Number(n.into()));
                    }
                    _ => {
                        errs.push((rule.name.clone(), "invalid"));
                    }
                },
            },
            FieldKind::Enum(allowed) => match get_trimmed(fields, &rule.name) {
                None => {
                    if rule.required {
                        errs.push((rule.name.clone(), "required"));
                    }
                }
                Some(val) if allowed.iter().any(|a| a == val) => {
                    values.insert(rule.name.clone(), Value::String(val.to_string()));
                }
                Some(_) => {
                    errs.push((rule.name.clone(), "invalid"));
                }
            },
            FieldKind::Route { allowed } => match get_trimmed(fields, &rule.name) {
                None => {
                    if rule.required {
                        errs.push((rule.name.clone(), "required"));
                    }
                }
                Some(r) => {
                    let ok = is_valid_route(r)
                        && allowed
                            .as_ref()
                            .is_none_or(|list| list.iter().any(|a| a == r));
                    if ok {
                        let route_str = r.to_string();
                        if rule.name == "source_route" {
                            source_route = Some(route_str.clone());
                        }
                        values.insert(rule.name.clone(), Value::String(route_str));
                    } else {
                        errs.push((rule.name.clone(), "invalid"));
                    }
                }
            },
        }
    }

    let turnstile_token = validate_token_field(fields, &mut errs);

    if !errs.is_empty() {
        return Err(ValidationError::Fields(errs));
    }

    let resolved_route = source_route.unwrap_or_else(|| {
        get_trimmed(fields, "source_route")
            .filter(|r| is_valid_route(r))
            .map(str::to_string)
            .unwrap_or_else(|| "/".to_string())
    });

    Ok(ValidatedSubmission {
        form_id: schema.id.clone(),
        source_route: resolved_route,
        values,
        turnstile_token,
    })
}

pub fn default_form_schemas() -> BTreeMap<String, FormSchema> {
    let mut schemas = BTreeMap::new();

    schemas.insert(
        "contact".to_string(),
        FormSchema {
            id: "contact".to_string(),
            turnstile_action: "contact".to_string(),
            fields: vec![
                FormFieldRule {
                    name: "name".to_string(),
                    required: true,
                    kind: FieldKind::Text {
                        max_chars: 100,
                        multiline: false,
                    },
                },
                FormFieldRule {
                    name: "email".to_string(),
                    required: true,
                    kind: FieldKind::Email,
                },
                FormFieldRule {
                    name: "phone".to_string(),
                    required: false,
                    kind: FieldKind::Phone { max_chars: 32 },
                },
                FormFieldRule {
                    name: "message".to_string(),
                    required: true,
                    kind: FieldKind::Text {
                        max_chars: 4000,
                        multiline: true,
                    },
                },
                FormFieldRule {
                    name: "source_route".to_string(),
                    required: true,
                    kind: FieldKind::Route { allowed: None },
                },
            ],
        },
    );

    schemas
}

/// Severity of a form-schema loading diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormSchemaDiagnosticLevel {
    /// Load-blocking problem (parse failure): the override was discarded.
    Error,
    /// A single malformed entry was skipped; the rest of the override applied.
    Warning,
}

/// Diagnostic emitted while loading `FORM_SCHEMAS_JSON`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormSchemaDiagnostic {
    pub level: FormSchemaDiagnosticLevel,
    pub code: &'static str,
    pub message: String,
}

/// Parse an optional `FORM_SCHEMAS_JSON` override and merge it over `default_form_schemas()`.
///
/// Returns the merged schemas plus a deterministic diagnostics list describing
/// any parse failure (override discarded, defaults used) or skipped entry so
/// callers can log it instead of failing silently.
pub fn load_form_schemas(
    json_override: Option<&str>,
) -> (BTreeMap<String, FormSchema>, Vec<FormSchemaDiagnostic>) {
    let mut schemas = default_form_schemas();
    let mut diagnostics = Vec::new();
    let Some(raw) = json_override.map(str::trim).filter(|s| !s.is_empty()) else {
        return (schemas, diagnostics);
    };
    let parsed = match serde_json::from_str::<BTreeMap<String, FormSchema>>(raw) {
        Ok(parsed) => parsed,
        Err(e) => {
            diagnostics.push(FormSchemaDiagnostic {
                level: FormSchemaDiagnosticLevel::Error,
                code: "form_schemas_parse_failed",
                message: format!("FORM_SCHEMAS_JSON invalid: {e}; falling back to default schemas"),
            });
            return (schemas, diagnostics);
        }
    };
    for (key, mut schema) in parsed {
        if !is_valid_form_id(&key) {
            diagnostics.push(FormSchemaDiagnostic {
                level: FormSchemaDiagnosticLevel::Warning,
                code: "form_schema_invalid_id",
                message: format!(
                    "FORM_SCHEMAS_JSON entry '{key}' skipped: not a valid form id"
                ),
            });
            continue;
        }
        if schema.id.is_empty() {
            schema.id = key.clone();
        } else if schema.id != key {
            diagnostics.push(FormSchemaDiagnostic {
                level: FormSchemaDiagnosticLevel::Warning,
                code: "form_schema_id_mismatch",
                message: format!(
                    "FORM_SCHEMAS_JSON entry '{key}' skipped: schema id '{}' does not match its map key",
                    schema.id
                ),
            });
            continue;
        }
        if schema.turnstile_action.is_empty() {
            schema.turnstile_action = key.clone();
        }
        schemas.insert(key, schema);
    }
    (schemas, diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sws_security::validate::{MAX_TURNSTILE_TOKEN, TURNSTILE_FIELD};

    fn fields(pairs: &[(&str, &str)]) -> Fields {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn good_inquire() -> Fields {
        fields(&[
            ("kind", "private_event"),
            ("name", "Alex Example"),
            ("email", "Alex@Example.test"),
            ("phone", "+1 (703) 555-0100"),
            ("event_date", "2027-02-28"),
            ("guest_count", "24"),
            ("message", "Birthday dinner\nfor 24."),
            ("source_route", "/book-an-event"),
            (TURNSTILE_FIELD, "XXXX.DUMMY.TOKEN.XXXX"),
        ])
    }

    fn good_contact() -> Fields {
        fields(&[
            ("name", "Sam Example"),
            ("email", "Sam@Example.test"),
            ("message", "Hello there."),
            ("source_route", "/about"),
            (TURNSTILE_FIELD, "XXXX.DUMMY.TOKEN.XXXX"),
        ])
    }

    const HOSPITALITY_INQUIRE_JSON: &str = r#"{
        "inquire": {
            "id": "inquire",
            "turnstile_action": "inquire",
            "fields": [
                { "name": "kind", "required": true, "kind": { "enum": ["private_event", "contact"] } },
                { "name": "name", "required": true, "kind": { "text": { "max_chars": 100, "multiline": false } } },
                { "name": "email", "required": true, "kind": "email" },
                { "name": "phone", "required": false, "kind": { "phone": { "max_chars": 32 } } },
                { "name": "event_date", "required": false, "kind": "date" },
                { "name": "guest_count", "required": false, "kind": { "integer": { "min": 1, "max": 1000 } } },
                { "name": "message", "required": true, "kind": { "text": { "max_chars": 4000, "multiline": true } } },
                { "name": "source_route", "required": true, "kind": { "route": { "allowed": ["/book-an-event", "/contact"] } } }
            ]
        }
    }"#;

    #[test]
    fn default_schemas_are_domain_agnostic() {
        let schemas = default_form_schemas();
        assert!(schemas.contains_key("contact"));
        assert!(!schemas.contains_key("inquire"));
    }

    #[test]
    fn contact_valid_minimal_and_optional_phone() {
        let schemas = default_form_schemas();
        let contact = &schemas["contact"];

        let res = validate_submission(contact, &good_contact()).unwrap();
        assert_eq!(res.form_id, "contact");
        assert_eq!(res.source_route, "/about");
        assert_eq!(res.values["name"], "Sam Example");
        assert_eq!(res.values["email"], "sam@example.test");
        assert!(!res.values.contains_key("phone"));
        assert_eq!(res.values["message"], "Hello there.");
        assert_eq!(res.turnstile_token.as_deref(), Some("XXXX.DUMMY.TOKEN.XXXX"));

        let mut with_phone = good_contact();
        with_phone.insert("phone".into(), "+1 (555) 010-1234".into());
        let res2 = validate_submission(contact, &with_phone).unwrap();
        assert_eq!(res2.values["phone"], "+1 (555) 010-1234");
    }

    #[test]
    fn contact_rejects_bad_fields() {
        let schemas = default_form_schemas();
        let contact = &schemas["contact"];

        let Err(ValidationError::Fields(errs)) = validate_submission(contact, &Fields::new()) else {
            panic!("expected FieldErrors");
        };
        for key in ["name", "email", "message", "source_route"] {
            assert!(
                errs.contains(&(key.to_string(), "required")),
                "missing required for {key}: {errs:?}"
            );
        }

        let mut bad_route = good_contact();
        bad_route.insert("source_route".into(), "//evil.example".into());
        let Err(ValidationError::Fields(errs)) = validate_submission(contact, &bad_route) else {
            panic!("expected FieldErrors");
        };
        assert!(errs.contains(&("source_route".to_string(), "invalid")));
    }

    #[test]
    fn inquire_valid_normalizes() {
        let (schemas, _) = load_form_schemas(Some(HOSPITALITY_INQUIRE_JSON));
        let inquire = &schemas["inquire"];

        let mut f = good_inquire();
        f.insert("phone".into(), "+1 (703) 555-0100 X42".into());
        let res = validate_submission(inquire, &f).unwrap();
        assert_eq!(res.form_id, "inquire");
        assert_eq!(res.source_route, "/book-an-event");
        assert_eq!(res.values["kind"], "private_event");
        assert_eq!(res.values["email"], "alex@example.test");
        assert_eq!(res.values["phone"], "+1 (703) 555-0100 X42");
        assert_eq!(res.values["event_date"], "2027-02-28");
        assert_eq!(res.values["guest_count"], serde_json::json!(24));
        assert_eq!(res.turnstile_token.as_deref(), Some("XXXX.DUMMY.TOKEN.XXXX"));
    }

    #[test]
    fn inquire_minimal_contact() {
        let (schemas, _) = load_form_schemas(Some(HOSPITALITY_INQUIRE_JSON));
        let inquire = &schemas["inquire"];

        let f = fields(&[
            ("kind", "contact"),
            ("name", "Sam"),
            ("email", "sam@example.test"),
            ("message", "Hi"),
            ("source_route", "/contact"),
        ]);
        let res = validate_submission(inquire, &f).unwrap();
        assert!(!res.values.contains_key("phone"));
        assert!(!res.values.contains_key("event_date"));
        assert!(!res.values.contains_key("guest_count"));
        assert_eq!(res.turnstile_token, None);
    }

    #[test]
    fn inquire_rejections_all_rules() {
        let (schemas, _) = load_form_schemas(Some(HOSPITALITY_INQUIRE_JSON));
        let inquire = &schemas["inquire"];

        let cases: &[(&str, &str, &str)] = &[
            ("kind", "catering", "invalid"),
            ("email", "nope", "invalid"),
            ("phone", "call me", "invalid"),
            ("phone", "123-456", "invalid"),
            ("phone", "() +-.", "invalid"),
            ("event_date", "2027-02-30", "invalid"),
            ("event_date", "2027-02-300", "too_long"),
            ("guest_count", "0", "invalid"),
            ("guest_count", "1001", "invalid"),
            ("guest_count", "-3", "invalid"),
            ("guest_count", "twenty", "invalid"),
            ("source_route", "/elsewhere", "invalid"),
            ("name", "bad\u{0007}name", "invalid_characters"),
        ];

        for (k, v, reason) in cases {
            let mut f = good_inquire();
            f.insert((*k).to_string(), (*v).to_string());
            let ValidationError::Fields(errs) = validate_submission(inquire, &f).unwrap_err() else {
                panic!("expected FieldErrors for {k}={v}");
            };
            assert!(
                errs.contains(&((*k).to_string(), *reason)),
                "{k}={v}: {errs:?}"
            );
        }

        // guest_count boundaries 1 and 1000 are accepted
        for boundary in ["1", "1000"] {
            let mut f = good_inquire();
            f.insert("guest_count".into(), boundary.into());
            assert!(validate_submission(inquire, &f).is_ok(), "boundary {boundary}");
        }

        // message too_long
        let mut f = good_inquire();
        f.insert("message".into(), "x".repeat(4001));
        assert!(matches!(
            validate_submission(inquire, &f).unwrap_err(),
            ValidationError::Fields(errs) if errs.contains(&("message".to_string(), "too_long"))
        ));

        // turnstile token too_long
        let mut f = good_inquire();
        f.insert(TURNSTILE_FIELD.into(), "t".repeat(MAX_TURNSTILE_TOKEN + 1));
        assert!(matches!(
            validate_submission(inquire, &f).unwrap_err(),
            ValidationError::Fields(errs) if errs.contains(&(TURNSTILE_FIELD.to_string(), "too_long"))
        ));

        // all required missing
        let ValidationError::Fields(errs) =
            validate_submission(inquire, &Fields::new()).unwrap_err()
        else {
            panic!("expected FieldErrors for empty inquire");
        };
        for k in ["kind", "name", "email", "message", "source_route"] {
            assert!(
                errs.contains(&(k.to_string(), "required")),
                "missing required for {k}: {errs:?}"
            );
        }
    }

    #[test]
    fn honeypot_short_circuits() {
        let (schemas, _) = load_form_schemas(Some(HOSPITALITY_INQUIRE_JSON));
        let mut f = good_inquire();
        f.insert(HONEYPOT_FIELD.into(), "https://spam.example".into());
        assert_eq!(
            validate_submission(&schemas["inquire"], &f).unwrap_err(),
            ValidationError::Honeypot
        );

        let mut c = good_contact();
        c.insert(HONEYPOT_FIELD.into(), "bot".into());
        assert_eq!(
            validate_submission(&schemas["contact"], &c).unwrap_err(),
            ValidationError::Honeypot
        );
    }

    #[test]
    fn load_form_schemas_merges_and_falls_back_safely() {
        // None / blank / invalid JSON -> defaults only, no diagnostics
        let base = default_form_schemas();
        for input in [None, Some("   ")] {
            let (schemas, diagnostics) = load_form_schemas(input);
            assert_eq!(schemas, base);
            assert!(diagnostics.is_empty(), "{diagnostics:?}");
        }
        for input in ["{not valid json", "[1, 2, 3]"] {
            let (schemas, diagnostics) = load_form_schemas(Some(input));
            assert_eq!(schemas, base);
            assert!(
                diagnostics
                    .iter()
                    .any(|d| d.code == "form_schemas_parse_failed"
                        && d.level == FormSchemaDiagnosticLevel::Error),
                "{input}: {diagnostics:?}"
            );
            assert!(
                diagnostics[0].message.contains("FORM_SCHEMAS_JSON invalid")
                    && diagnostics[0].message.contains("falling back to default schemas"),
                "{diagnostics:?}"
            );
        }

        // Garbage input logs a diagnostic and falls back to defaults
        let (fallback, diagnostics) = load_form_schemas(Some("]]>not-json<[["));
        assert_eq!(fallback, base);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "form_schemas_parse_failed");
        assert!(
            diagnostics[0].message.contains("FORM_SCHEMAS_JSON invalid"),
            "{diagnostics:?}"
        );

        // Valid custom schema + override contact + invalid form_id ignored
        let json = r#"{
            "lead": {
                "id": "lead",
                "turnstile_action": "lead_submit",
                "fields": [
                    { "name": "company", "required": true, "kind": { "text": { "max_chars": 80, "multiline": false } } },
                    { "name": "work_email", "required": false, "kind": "email" },
                    { "name": "seats", "required": true, "kind": { "integer": { "min": 1, "max": 50 } } }
                ]
            },
            "contact": {
                "fields": [
                    { "name": "name", "required": true, "kind": { "text": { "max_chars": 50, "multiline": false } } }
                ]
            },
            "INVALID_ID": {
                "fields": []
            }
        }"#;

        let (loaded, loaded_diags) = load_form_schemas(Some(json));
        assert!(!loaded.contains_key("inquire"));
        assert!(loaded.contains_key("lead"));
        assert!(!loaded.contains_key("INVALID_ID"));
        assert_eq!(loaded["lead"].turnstile_action, "lead_submit");
        // Omitted id/turnstile_action defaulted from map key
        assert_eq!(loaded["contact"].id, "contact");
        assert_eq!(loaded["contact"].turnstile_action, "contact");
        assert_eq!(loaded["contact"].fields.len(), 1);
        // Invalid form id entry is warned, not silently dropped
        assert!(
            loaded_diags
                .iter()
                .any(|d| d.code == "form_schema_invalid_id"
                    && d.message.contains("'INVALID_ID'")),
            "{loaded_diags:?}"
        );

        // Validate against custom schema
        let lead_fields = fields(&[
            ("company", "Acme Studio"),
            ("work_email", "Team@Acme.test"),
            ("seats", "10"),
        ]);
        let sub = validate_submission(&loaded["lead"], &lead_fields).unwrap();
        assert_eq!(sub.form_id, "lead");
        assert_eq!(sub.source_route, "/");
        assert_eq!(sub.values["company"], "Acme Studio");
        assert_eq!(sub.values["work_email"], "team@acme.test");
        assert_eq!(sub.values["seats"], serde_json::json!(10));

        // Mismatched schema.id must be ignored so submission form_id matches URL key.
        let mismatched = r#"{
            "feedback": {
                "id": "other_form",
                "fields": []
            }
        }"#;
        let (loaded_mismatch, mismatch_diags) = load_form_schemas(Some(mismatched));
        assert!(!loaded_mismatch.contains_key("feedback"));
        assert!(
            mismatch_diags
                .iter()
                .any(|d| d.code == "form_schema_id_mismatch"
                    && d.level == FormSchemaDiagnosticLevel::Warning
                    && d.message.contains("'feedback'")
                    && d.message.contains("other_form")),
            "{mismatch_diags:?}"
        );
    }
}
