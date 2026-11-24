use db::time::to_micros;
use domain::CustomerId;
use jiff::Timestamp;
use sqlx::PgConnection;

use crate::Error;

const MAX_EMAIL_LEN: usize = 254;
const MAX_NAME_LEN: usize = 200;
const MAX_PHONE_LEN: usize = 40;

/// What a guest enters at checkout (F4). No address: everything is picked up.
/// `marketing_opt_in` is only a wish kept on the order; subscribing needs the double
/// opt-in of F9a.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomerInput {
    pub email: String,
    pub name: String,
    pub phone: Option<String>,
    pub marketing_opt_in: bool,
}

/// Checked, trimmed customer details; the email is lower-cased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Valid {
    pub email: String,
    pub name: String,
    pub phone: Option<String>,
}

fn plain_text(text: &str, max_chars: usize) -> bool {
    !text.is_empty() && text.chars().count() <= max_chars && !text.chars().any(char::is_control)
}

/// A loose shape check of the email only: something before and after one `@`, no
/// spaces. The confirmation email is the real test. Name and phone are single lines.
pub(crate) fn validate(input: &CustomerInput) -> Result<Valid, Error> {
    let email = input.email.trim().to_lowercase();
    let well_formed = email.len() <= MAX_EMAIL_LEN
        && !email.chars().any(|c| c.is_whitespace() || c.is_control())
        && email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && domain.contains('.') && !domain.contains('@')
        });
    if !well_formed {
        return Err(Error::Invalid("email"));
    }
    let name = input.name.trim();
    if !plain_text(name, MAX_NAME_LEN) {
        return Err(Error::Invalid("name"));
    }
    let phone = input
        .phone
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty());
    if phone.is_some_and(|p| !plain_text(p, MAX_PHONE_LEN)) {
        return Err(Error::Invalid("phone"));
    }
    Ok(Valid {
        email,
        name: name.to_owned(),
        phone: phone.map(str::to_owned),
    })
}

/// Finds the customer by email, or creates them from these details. A guest checkout
/// never changes an existing customer: anyone can type any email address. The order
/// keeps the name and phone entered for it. The no-op update locks the row until the
/// checkout commits, so an erase (F12) waits and then sees the new order.
pub(crate) async fn find_or_create(
    conn: &mut PgConnection,
    valid: &Valid,
    now: Timestamp,
) -> Result<CustomerId, Error> {
    let id = sqlx::query_scalar!(
        "INSERT INTO customer (email, name, phone, created_at)
        VALUES ($1, $2, $3, timestamptz 'epoch' + $4::bigint * interval '1 microsecond')
        ON CONFLICT (email) DO UPDATE SET email = EXCLUDED.email
        RETURNING id",
        valid.email,
        valid.name,
        valid.phone,
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
            marketing_opt_in: false,
        }
    }

    #[test]
    fn input_is_trimmed_and_the_email_lowercased() {
        let ok = input(" Jan@Example.ORG ", " Jan \n", Some(" "));
        assert_eq!(
            validate(&ok).unwrap(),
            Valid {
                email: "jan@example.org".to_owned(),
                name: "Jan".to_owned(),
                phone: None
            }
        );
        assert_eq!(
            validate(&input("a@b.nl", "A", Some(" 06 1234 ")))
                .unwrap()
                .phone
                .as_deref(),
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
            "a\u{0}@b.nl",
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
        for name in [" ", &"x".repeat(201), "Jan\nAdmin", "Jan\u{7}", "Jan\r\nX"] {
            assert!(
                matches!(
                    validate(&input("a@b.nl", name, None)),
                    Err(Error::Invalid("name"))
                ),
                "{name:?}"
            );
        }
        for phone in [&"1".repeat(41), "06\n123", "06\t123"] {
            assert!(
                matches!(
                    validate(&input("a@b.nl", "Jan", Some(phone))),
                    Err(Error::Invalid("phone"))
                ),
                "{phone:?}"
            );
        }
    }
}
