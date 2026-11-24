use domain::ProviderPaymentId;

const MAX_BODY: usize = 1024;
const MAX_ID_SUFFIX: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WebhookError {
    #[error("webhook body is larger than {MAX_BODY} bytes")]
    TooLarge,
    #[error("webhook body has no id")]
    MissingId,
    #[error("webhook body has more than one id")]
    DuplicateId,
    #[error("webhook id is not a Mollie payment id")]
    InvalidId,
}

/// Reads the payment id from a Mollie payment webhook (`id=tr_…`, form encoded).
///
/// The body says nothing else that can be trusted: fetch the payment with
/// [`domain::PaymentProvider::get_payment`] before changing an order.
///
/// # Errors
/// [`WebhookError`] when the body is too large, or has no, several or a malformed id.
pub fn parse_webhook_body(body: &[u8]) -> Result<ProviderPaymentId, WebhookError> {
    if body.len() > MAX_BODY {
        return Err(WebhookError::TooLarge);
    }
    let mut id = None;
    for pair in body.split(|&b| b == b'&') {
        let (key, value) = match pair.iter().position(|&b| b == b'=') {
            Some(at) => (&pair[..at], &pair[at + 1..]),
            None => (pair, &b""[..]),
        };
        if key != b"id" {
            continue;
        }
        if id.replace(value).is_some() {
            return Err(WebhookError::DuplicateId);
        }
    }
    let value = id.ok_or(WebhookError::MissingId)?;
    let suffix = value.strip_prefix(b"tr_").ok_or(WebhookError::InvalidId)?;
    if suffix.is_empty()
        || suffix.len() > MAX_ID_SUFFIX
        || !suffix.iter().all(u8::is_ascii_alphanumeric)
    {
        return Err(WebhookError::InvalidId);
    }
    let text: String = value.iter().copied().map(char::from).collect();
    ProviderPaymentId::new(text).map_err(|_| WebhookError::InvalidId)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn reads_the_id() {
        let id = parse_webhook_body(b"id=tr_d0b0E3EA3v").unwrap();
        assert_eq!(id.as_str(), "tr_d0b0E3EA3v");
        let id = parse_webhook_body(b"foo=bar&id=tr_5B8cwPMGnU&x").unwrap();
        assert_eq!(id.as_str(), "tr_5B8cwPMGnU");
    }

    #[test]
    fn refuses_bad_bodies() {
        let cases: [(&[u8], WebhookError); 13] = [
            (b"", WebhookError::MissingId),
            (b"ID=tr_1", WebhookError::MissingId),
            (b"payment=tr_1", WebhookError::MissingId),
            (b"id=tr_1&id=tr_2", WebhookError::DuplicateId),
            (b"id", WebhookError::InvalidId),
            (b"id=", WebhookError::InvalidId),
            (b"id=tr_", WebhookError::InvalidId),
            (b"id=re_abc", WebhookError::InvalidId),
            (b"id=ord_abc", WebhookError::InvalidId),
            (b"id=tr_abc%20", WebhookError::InvalidId),
            (b"id=tr_a+b", WebhookError::InvalidId),
            (b"id=tr_a_b", WebhookError::InvalidId),
            (
                b"id=tr_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                WebhookError::InvalidId,
            ),
        ];
        for (body, err) in cases {
            assert_eq!(
                parse_webhook_body(body),
                Err(err),
                "{}",
                String::from_utf8_lossy(body)
            );
        }
        assert_eq!(
            parse_webhook_body(&[b'a'; MAX_BODY + 1]),
            Err(WebhookError::TooLarge)
        );
        assert_eq!(
            parse_webhook_body(b"id=tr_\xff"),
            Err(WebhookError::InvalidId)
        );
    }

    proptest! {
        #[test]
        fn never_panics(body in proptest::collection::vec(any::<u8>(), 0..2048)) {
            let _ = parse_webhook_body(&body);
        }

        #[test]
        fn accepts_any_well_formed_id(suffix in "[A-Za-z0-9]{1,32}") {
            let body = format!("id=tr_{suffix}");
            let id = parse_webhook_body(body.as_bytes()).unwrap();
            prop_assert_eq!(id.as_str(), &body[3..]);
        }
    }
}
