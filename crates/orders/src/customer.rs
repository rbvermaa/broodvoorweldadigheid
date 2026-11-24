use db::time::to_micros;
use domain::CustomerId;
use jiff::Timestamp;
use sqlx::PgConnection;

use crate::Error;

const MAX_EMAIL_LEN: usize = 254;
const MAX_NAME_LEN: usize = 200;
const MAX_PHONE_LEN: usize = 40;

/// What a guest enters at checkout (F4). No address: everything is picked up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomerInput {
    pub email: String,
    pub name: String,
    pub phone: Option<String>,
    pub marketing_consent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Valid<'a> {
    email: String,
    name: &'a str,
    phone: Option<&'a str>,
}

/// A loose shape check only: something before and after one `@`, no spaces. The
/// confirmation email is the real test.
fn validate(input: &CustomerInput) -> Result<Valid<'_>, Error> {
    let email = input.email.trim().to_lowercase();
    let well_formed = email.len() <= MAX_EMAIL_LEN
        && !email.chars().any(char::is_whitespace)
        && email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && domain.contains('.') && !domain.contains('@')
        });
    if !well_formed {
        return Err(Error::Invalid("email"));
    }
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_LEN {
        return Err(Error::Invalid("name"));
    }
    let phone = input
        .phone
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty());
    if phone.is_some_and(|p| p.chars().count() > MAX_PHONE_LEN) {
        return Err(Error::Invalid("phone"));
    }
    Ok(Valid { email, name, phone })
}

/// Finds the guest by email or creates them. A returning guest's name and phone are
/// replaced by what they entered now. Marketing consent is only ever switched on here;
/// withdrawing it is a separate action (F12).
pub(crate) async fn upsert(
    conn: &mut PgConnection,
    input: &CustomerInput,
    now: Timestamp,
) -> Result<CustomerId, Error> {
    let valid = validate(input)?;
    let id = sqlx::query_scalar!(
        "INSERT INTO customer (email, name, phone, marketing_consent, created_at)
        VALUES ($1, $2, $3, $4, timestamptz 'epoch' + $5::bigint * interval '1 microsecond')
        ON CONFLICT (email) DO UPDATE
        SET name = EXCLUDED.name, phone = EXCLUDED.phone,
            marketing_consent = customer.marketing_consent OR EXCLUDED.marketing_consent
        RETURNING id",
        valid.email,
        valid.name,
        valid.phone,
        input.marketing_consent,
        to_micros(now),
    )
    .fetch_one(conn)
    .await?;
    Ok(CustomerId::new(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(email: &str, name: &str, phone: Option<&str>) -> CustomerInput {
        CustomerInput {
            email: email.to_owned(),
            name: name.to_owned(),
            phone: phone.map(str::to_owned),
            marketing_consent: false,
        }
    }

    #[test]
    fn input_is_trimmed_and_the_email_lowercased() {
        let ok = input(" Jan@Example.ORG ", " Jan ", Some(" "));
        assert_eq!(
            validate(&ok).unwrap(),
            Valid {
                email: "jan@example.org".to_owned(),
                name: "Jan",
                phone: None
            }
        );
        assert_eq!(
            validate(&input("a@b.nl", "A", Some(" 06 1234 ")))
                .unwrap()
                .phone,
            Some("06 1234")
        );
    }

    #[test]
    fn bad_input_is_refused() {
        let long = "x".repeat(255);
        for email in [
            "",
            "jan",
            "@example.org",
            "jan@example",
            "j an@example.org",
            "a@b@c.nl",
            &long,
        ] {
            assert!(
                matches!(
                    validate(&input(email, "Jan", None)),
                    Err(Error::Invalid("email"))
                ),
                "{email}"
            );
        }
        assert!(matches!(
            validate(&input("a@b.nl", " ", None)),
            Err(Error::Invalid("name"))
        ));
        assert!(matches!(
            validate(&input("a@b.nl", &"x".repeat(201), None)),
            Err(Error::Invalid("name"))
        ));
        assert!(matches!(
            validate(&input("a@b.nl", "Jan", Some(&"1".repeat(41)))),
            Err(Error::Invalid("phone"))
        ));
    }
}
