//! Hospitality inquiry parsing and validation (M3/M4: private events and contact).

use sws_security::validate::{
    FieldErrors, Fields, HONEYPOT_FIELD, ValidationError, get_trimmed, is_valid_date,
    is_valid_phone, validate_email_field, validate_text_field, validate_token_field,
};

pub const INQUIRY_ROUTES: &[&str] = &["/book-an-event", "/contact"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InquiryKind {
    PrivateEvent,
    Contact,
}

impl InquiryKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            InquiryKind::PrivateEvent => "private_event",
            InquiryKind::Contact => "contact",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inquiry {
    pub kind: InquiryKind,
    pub name: String,
    pub email: String,
    pub phone: Option<String>,
    pub event_date: Option<String>,
    pub guest_count: Option<u32>,
    pub message: String,
    pub source_route: String,
    pub turnstile_token: Option<String>,
}

/// Validate a hospitality inquiry submission (`private_event` or `contact`).
pub fn validate_inquiry(f: &Fields) -> Result<Inquiry, ValidationError> {
    if get_trimmed(f, HONEYPOT_FIELD).is_some() {
        return Err(ValidationError::Honeypot);
    }
    let mut errs = FieldErrors::new();
    let kind = match get_trimmed(f, "kind") {
        Some("private_event") => Some(InquiryKind::PrivateEvent),
        Some("contact") => Some(InquiryKind::Contact),
        Some(_) => {
            errs.push(("kind".to_string(), "invalid"));
            None
        }
        None => {
            errs.push(("kind".to_string(), "required"));
            None
        }
    };
    let name = validate_text_field(f, "name", 100, true, false, &mut errs);
    let email = validate_email_field(f, &mut errs);
    let phone = validate_text_field(f, "phone", 32, false, false, &mut errs);
    if let Some(p) = &phone
        && !is_valid_phone(p)
    {
        errs.push(("phone".to_string(), "invalid"));
    }
    let event_date = validate_text_field(f, "event_date", 10, false, false, &mut errs);
    if let Some(d) = &event_date
        && !is_valid_date(d)
    {
        errs.push(("event_date".to_string(), "invalid"));
    }
    let guest_count = match get_trimmed(f, "guest_count") {
        None => None,
        Some(g) => match g.parse::<u32>() {
            Ok(n) if (1..=1000).contains(&n) => Some(n),
            _ => {
                errs.push(("guest_count".to_string(), "invalid"));
                None
            }
        },
    };
    let message = validate_text_field(f, "message", 4000, true, true, &mut errs);
    let source_route = match get_trimmed(f, "source_route") {
        Some(r) if INQUIRY_ROUTES.contains(&r) => Some(r.to_string()),
        Some(_) => {
            errs.push(("source_route".to_string(), "invalid"));
            None
        }
        None => {
            errs.push(("source_route".to_string(), "required"));
            None
        }
    };
    let turnstile_token = validate_token_field(f, &mut errs);
    match (kind, name, email, message, source_route) {
        (Some(kind), Some(name), Some(email), Some(message), Some(source_route))
            if errs.is_empty() =>
        {
            Ok(Inquiry {
                kind,
                name,
                email,
                phone,
                event_date,
                guest_count,
                message,
                source_route,
                turnstile_token,
            })
        }
        _ => Err(ValidationError::Fields(errs)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sws_security::validate::TURNSTILE_FIELD;

    fn fields(pairs: &[(&str, &str)]) -> Fields {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn good_inquiry() -> Fields {
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

    #[test]
    fn valid_inquiry_normalizes() {
        let mut f = good_inquiry();
        f.insert("phone".into(), "+1 (703) 555-0100 X42".into());
        let i = validate_inquiry(&f).unwrap();
        assert_eq!(i.kind, InquiryKind::PrivateEvent);
        assert_eq!(i.kind.as_str(), "private_event");
        assert_eq!(i.email, "alex@example.test");
        assert_eq!(i.phone.as_deref(), Some("+1 (703) 555-0100 X42"));
        assert_eq!(i.guest_count, Some(24));
        assert_eq!(i.turnstile_token.as_deref(), Some("XXXX.DUMMY.TOKEN.XXXX"));
    }

    #[test]
    fn contact_inquiry_minimal() {
        let f = fields(&[
            ("kind", "contact"),
            ("name", "Sam"),
            ("email", "sam@example.test"),
            ("message", "Hi"),
            ("source_route", "/contact"),
        ]);
        let i = validate_inquiry(&f).unwrap();
        assert_eq!(i.kind.as_str(), "contact");
        assert_eq!(
            (i.phone, i.event_date, i.guest_count, i.turnstile_token),
            (None, None, None, None)
        );
    }

    #[test]
    fn inquiry_rejections() {
        let cases: &[(&str, &str, &str)] = &[
            ("kind", "catering", "invalid"),
            ("email", "nope", "invalid"),
            ("phone", "call me", "invalid"),
            ("phone", "123-456", "invalid"),
            ("phone", "() +-.", "invalid"),
            ("event_date", "2027-02-30", "invalid"),
            ("guest_count", "0", "invalid"),
            ("guest_count", "-3", "invalid"),
            ("source_route", "/elsewhere", "invalid"),
            ("name", "bad\u{0007}name", "invalid_characters"),
        ];
        for (k, v, reason) in cases {
            let mut f = good_inquiry();
            f.insert((*k).to_string(), (*v).to_string());
            let ValidationError::Fields(errs) = validate_inquiry(&f).unwrap_err() else {
                panic!("expected FieldErrors for {k}={v}");
            };
            assert!(
                errs.contains(&((*k).to_string(), *reason)),
                "{k}={v}: {errs:?}"
            );
        }
        let mut f = good_inquiry();
        f.insert("message".into(), "x".repeat(4001));
        assert!(matches!(
            validate_inquiry(&f).unwrap_err(),
            ValidationError::Fields(errs) if errs.contains(&("message".to_string(), "too_long"))
        ));
        let ValidationError::Fields(errs) = validate_inquiry(&Fields::new()).unwrap_err() else {
            panic!("expected FieldErrors for empty inquiry");
        };
        for k in ["kind", "name", "email", "message", "source_route"] {
            assert!(errs.contains(&(k.to_string(), "required")), "{k}");
        }
    }

    #[test]
    fn honeypot_returns_dedicated_variant() {
        let mut f = good_inquiry();
        f.insert(HONEYPOT_FIELD.into(), "http://spam.test".into());
        assert_eq!(validate_inquiry(&f).unwrap_err(), ValidationError::Honeypot);
    }
}
