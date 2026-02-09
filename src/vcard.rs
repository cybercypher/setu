//! Convert a Google People API `Person` into a vCard 3.0 (RFC 2426) string.

use google_people1::api::Person;

/// Escape special characters for vCard text values.
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\n', "\\n")
}

/// Build a vCard 3.0 string from a Google `Person`.
///
/// `resource_name` is used as the UID (e.g. `people/c1234567890`).
pub fn person_to_vcard(person: &Person) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(20);

    lines.push("BEGIN:VCARD".into());
    lines.push("VERSION:3.0".into());

    // ── UID ──────────────────────────────────────────────────────
    let uid = person
        .resource_name
        .as_deref()
        .unwrap_or("unknown")
        .replace('/', "-");
    lines.push(format!("UID:{uid}"));

    // ── N / FN ───────────────────────────────────────────────────
    if let Some(names) = person.names.as_ref() {
        if let Some(n) = names.first() {
            let family = n.family_name.as_deref().unwrap_or("");
            let given = n.given_name.as_deref().unwrap_or("");
            let middle = n.middle_name.as_deref().unwrap_or("");
            let prefix = n.honorific_prefix.as_deref().unwrap_or("");
            let suffix = n.honorific_suffix.as_deref().unwrap_or("");
            lines.push(format!(
                "N:{};{};{};{};{}",
                escape(family),
                escape(given),
                escape(middle),
                escape(prefix),
                escape(suffix)
            ));
            let display = n
                .display_name
                .as_deref()
                .unwrap_or_else(|| {
                    // Won't be used as deref, build a owned string below instead
                    ""
                });
            if !display.is_empty() {
                lines.push(format!("FN:{}", escape(display)));
            } else {
                let fallback = format!("{given} {family}").trim().to_string();
                lines.push(format!("FN:{}", escape(&fallback)));
            }
        }
    } else {
        lines.push("N:;;;;".into());
        lines.push("FN:".into());
    }

    // ── EMAIL ────────────────────────────────────────────────────
    if let Some(emails) = person.email_addresses.as_ref() {
        for email in emails {
            let addr = email.value.as_deref().unwrap_or("");
            if addr.is_empty() {
                continue;
            }
            let type_param = match email.type_.as_deref() {
                Some("home") => "HOME",
                Some("work") => "WORK",
                _ => "INTERNET",
            };
            lines.push(format!("EMAIL;TYPE={type_param}:{addr}"));
        }
    }

    // ── TEL ──────────────────────────────────────────────────────
    if let Some(phones) = person.phone_numbers.as_ref() {
        for phone in phones {
            let number = phone.value.as_deref().unwrap_or("");
            if number.is_empty() {
                continue;
            }
            let type_param = match phone.type_.as_deref() {
                Some("mobile") => "CELL",
                Some("home") => "HOME",
                Some("work") => "WORK",
                Some("homeFax") | Some("workFax") => "FAX",
                _ => "VOICE",
            };
            lines.push(format!("TEL;TYPE={type_param}:{number}"));
        }
    }

    // ── ADR ──────────────────────────────────────────────────────
    if let Some(addrs) = person.addresses.as_ref() {
        for addr in addrs {
            let street = addr.street_address.as_deref().unwrap_or("");
            let city = addr.city.as_deref().unwrap_or("");
            let region = addr.region.as_deref().unwrap_or("");
            let postal = addr.postal_code.as_deref().unwrap_or("");
            let country = addr.country.as_deref().unwrap_or("");
            let type_param = match addr.type_.as_deref() {
                Some("home") => "HOME",
                Some("work") => "WORK",
                _ => "HOME",
            };
            // ADR: PO Box ; Extended ; Street ; City ; Region ; Postal ; Country
            lines.push(format!(
                "ADR;TYPE={type_param}:;;{};{};{};{};{}",
                escape(street),
                escape(city),
                escape(region),
                escape(postal),
                escape(country)
            ));
        }
    }

    // ── ORG / TITLE ──────────────────────────────────────────────
    if let Some(orgs) = person.organizations.as_ref() {
        if let Some(org) = orgs.first() {
            if let Some(name) = org.name.as_deref() {
                lines.push(format!("ORG:{}", escape(name)));
            }
            if let Some(title) = org.title.as_deref() {
                lines.push(format!("TITLE:{}", escape(title)));
            }
        }
    }

    // ── BDAY ─────────────────────────────────────────────────────
    if let Some(bdays) = person.birthdays.as_ref() {
        if let Some(bday) = bdays.first() {
            if let Some(date) = bday.date.as_ref() {
                let y = date.year.unwrap_or(0);
                let m = date.month.unwrap_or(0);
                let d = date.day.unwrap_or(0);
                if m > 0 && d > 0 {
                    if y > 0 {
                        lines.push(format!("BDAY:{y:04}-{m:02}-{d:02}"));
                    } else {
                        // Year unknown — use vCard 3.0 convention
                        lines.push(format!("BDAY:--{m:02}-{d:02}"));
                    }
                }
            }
        }
    }

    // ── PHOTO ────────────────────────────────────────────────────
    if let Some(photos) = person.photos.as_ref() {
        if let Some(photo) = photos.first() {
            if let Some(url) = photo.url.as_deref() {
                if photo.default.unwrap_or(false) {
                    // Skip Google's default silhouette
                } else {
                    lines.push(format!("PHOTO;VALUE=URI:{url}"));
                }
            }
        }
    }

    // ── REV (last modified) ──────────────────────────────────────
    // Google doesn't consistently expose modification time on Person,
    // so we use the current time.
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    lines.push(format!("REV:{now}"));

    lines.push("END:VCARD".into());

    // vCard line endings are CRLF
    lines.join("\r\n") + "\r\n"
}

/// Extract the display name from a Person (for the DB `display_name` column).
pub fn display_name(person: &Person) -> String {
    person
        .names
        .as_ref()
        .and_then(|names| names.first())
        .and_then(|n| n.display_name.clone())
        .unwrap_or_default()
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use google_people1::api::{
        Address, Birthday, Date, EmailAddress, Name, Organization, Person, PersonMetadata,
        PhoneNumber, Photo,
    };

    /// Build a fully-populated mock Person.
    fn mock_person() -> Person {
        Person {
            resource_name: Some("people/c1234567890".into()),
            etag: Some("abc123".into()),
            metadata: Some(PersonMetadata {
                deleted: Some(false),
                ..Default::default()
            }),
            names: Some(vec![Name {
                display_name: Some("Jane Doe".into()),
                family_name: Some("Doe".into()),
                given_name: Some("Jane".into()),
                middle_name: Some("M".into()),
                honorific_prefix: Some("Dr.".into()),
                honorific_suffix: Some("PhD".into()),
                ..Default::default()
            }]),
            email_addresses: Some(vec![
                EmailAddress {
                    value: Some("jane@example.com".into()),
                    type_: Some("home".into()),
                    ..Default::default()
                },
                EmailAddress {
                    value: Some("jane@work.com".into()),
                    type_: Some("work".into()),
                    ..Default::default()
                },
            ]),
            phone_numbers: Some(vec![PhoneNumber {
                value: Some("+1-555-0100".into()),
                type_: Some("mobile".into()),
                ..Default::default()
            }]),
            addresses: Some(vec![Address {
                street_address: Some("123 Main St".into()),
                city: Some("Springfield".into()),
                region: Some("IL".into()),
                postal_code: Some("62701".into()),
                country: Some("US".into()),
                type_: Some("home".into()),
                ..Default::default()
            }]),
            organizations: Some(vec![Organization {
                name: Some("Acme Corp".into()),
                title: Some("Engineer".into()),
                ..Default::default()
            }]),
            birthdays: Some(vec![Birthday {
                date: Some(Date {
                    year: Some(1990),
                    month: Some(3),
                    day: Some(15),
                }),
                ..Default::default()
            }]),
            photos: Some(vec![Photo {
                url: Some("https://lh3.google.com/photo.jpg".into()),
                default: Some(false),
                ..Default::default()
            }]),
            ..Default::default()
        }
    }

    #[test]
    fn vcard_has_required_structure() {
        let person = mock_person();
        let vcard = person_to_vcard(&person);

        // RFC 2426 required properties
        assert!(vcard.starts_with("BEGIN:VCARD\r\n"));
        assert!(vcard.ends_with("END:VCARD\r\n"));
        assert!(vcard.contains("VERSION:3.0\r\n"));
        assert!(vcard.contains("UID:people-c1234567890\r\n"));
    }

    #[test]
    fn vcard_uses_crlf_line_endings() {
        let person = mock_person();
        let vcard = person_to_vcard(&person);

        // Every line should end with \r\n, not bare \n.
        for line in vcard.split("\r\n") {
            assert!(
                !line.contains('\n'),
                "found bare LF in line: {line:?}"
            );
        }
    }

    #[test]
    fn vcard_name_fields() {
        let person = mock_person();
        let vcard = person_to_vcard(&person);

        // N:Family;Given;Middle;Prefix;Suffix
        assert!(vcard.contains("N:Doe;Jane;M;Dr.;PhD\r\n"));
        assert!(vcard.contains("FN:Jane Doe\r\n"));
    }

    #[test]
    fn vcard_email_types() {
        let person = mock_person();
        let vcard = person_to_vcard(&person);

        assert!(vcard.contains("EMAIL;TYPE=HOME:jane@example.com\r\n"));
        assert!(vcard.contains("EMAIL;TYPE=WORK:jane@work.com\r\n"));
    }

    #[test]
    fn vcard_phone() {
        let person = mock_person();
        let vcard = person_to_vcard(&person);
        assert!(vcard.contains("TEL;TYPE=CELL:+1-555-0100\r\n"));
    }

    #[test]
    fn vcard_address() {
        let person = mock_person();
        let vcard = person_to_vcard(&person);
        assert!(vcard.contains("ADR;TYPE=HOME:;;123 Main St;Springfield;IL;62701;US\r\n"));
    }

    #[test]
    fn vcard_org_and_title() {
        let person = mock_person();
        let vcard = person_to_vcard(&person);
        assert!(vcard.contains("ORG:Acme Corp\r\n"));
        assert!(vcard.contains("TITLE:Engineer\r\n"));
    }

    #[test]
    fn vcard_birthday() {
        let person = mock_person();
        let vcard = person_to_vcard(&person);
        assert!(vcard.contains("BDAY:1990-03-15\r\n"));
    }

    #[test]
    fn vcard_birthday_no_year() {
        let mut person = mock_person();
        person.birthdays = Some(vec![Birthday {
            date: Some(Date {
                year: None,
                month: Some(12),
                day: Some(25),
            }),
            ..Default::default()
        }]);
        let vcard = person_to_vcard(&person);
        assert!(vcard.contains("BDAY:--12-25\r\n"));
    }

    #[test]
    fn vcard_photo() {
        let person = mock_person();
        let vcard = person_to_vcard(&person);
        assert!(vcard.contains("PHOTO;VALUE=URI:https://lh3.google.com/photo.jpg\r\n"));
    }

    #[test]
    fn vcard_skips_default_photo() {
        let mut person = mock_person();
        person.photos = Some(vec![Photo {
            url: Some("https://lh3.google.com/default.jpg".into()),
            default: Some(true),
            ..Default::default()
        }]);
        let vcard = person_to_vcard(&person);
        assert!(!vcard.contains("PHOTO;"));
    }

    #[test]
    fn vcard_minimal_person() {
        let person = Person {
            resource_name: Some("people/c999".into()),
            ..Default::default()
        };
        let vcard = person_to_vcard(&person);
        assert!(vcard.contains("BEGIN:VCARD"));
        assert!(vcard.contains("N:;;;;"));
        assert!(vcard.contains("FN:"));
        assert!(vcard.contains("END:VCARD"));
    }

    #[test]
    fn vcard_escapes_special_chars() {
        let mut person = mock_person();
        person.names = Some(vec![Name {
            display_name: Some("O'Brien, Jr.".into()),
            family_name: Some("O'Brien, Jr.".into()),
            given_name: Some("Miles".into()),
            ..Default::default()
        }]);
        let vcard = person_to_vcard(&person);
        assert!(vcard.contains("N:O'Brien\\, Jr.;Miles;;;"));
        assert!(vcard.contains("FN:O'Brien\\, Jr."));
    }

    #[test]
    fn display_name_extraction() {
        let person = mock_person();
        assert_eq!(display_name(&person), "Jane Doe");

        let empty = Person::default();
        assert_eq!(display_name(&empty), "");
    }
}
