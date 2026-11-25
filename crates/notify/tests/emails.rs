#![forbid(unsafe_code)]

use domain::{Line, Money, VatRate, VatTotals};
use i18n::Translator;
use jiff::Timestamp;
use jiff::civil::{Date, date};
use notify::{
    Company, ComposeError, Composer, ContactMessage, Donation, DonationMoved, DropAlert,
    DropAlertKind, Email, EmailKind, Item, LoginLink, OnAccountConfirmation, OrderCancelled,
    OrderConfirmation, OrderLine, OutgoingEmail, Pickup, PickupMoved, PickupReminder, Recipient,
    SendError, SickDayCancelled, SubscriptionConfirmation,
};

const NBSP: char = '\u{a0}';

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn company() -> Company {
    Company {
        name: "Brood voor Weldadigheid".to_owned(),
        address: "Majoor van Swietenlaan 15, 8382 CE Frederiksoord".to_owned(),
        kvk: "12345678".to_owned(),
    }
}

fn i18n() -> Translator {
    Translator::dutch().expect("Dutch texts")
}

fn sanne() -> Recipient {
    Recipient {
        email: "sanne@example.org".to_owned(),
        name: Some("Sanne".to_owned()),
    }
}

/// Saturday 5 December 2026, 11:00–12:00 in Amsterdam (UTC+1).
fn pickup() -> Pickup {
    Pickup {
        starts: ts("2026-12-05T10:00:00Z"),
        ends: ts("2026-12-05T11:00:00Z"),
        location: "De Proef".to_owned(),
        address: "Majoor van Swietenlaan 15, 8382 CE Frederiksoord".to_owned(),
    }
}

const DROP: Date = date(2026, 12, 5);

fn line(product: &str, qty: u32, cents: i64, vat_rate: VatRate, pwyw: bool) -> OrderLine {
    OrderLine {
        product: product.to_owned(),
        qty,
        unit_price: Money::from_cents(cents),
        vat_rate,
        pay_what_you_want: pwyw,
    }
}

fn lines() -> Vec<OrderLine> {
    vec![
        line("Desem wit", 2, 650, VatRate::Nine, false),
        line("Volkoren spelt", 1, 725, VatRate::Nine, true),
        line("Theedoek", 1, 1250, VatRate::TwentyOne, false),
    ]
}

fn confirmation(donation: Donation) -> OrderConfirmation {
    OrderConfirmation {
        to: sanne(),
        order_number: "2026-0042".to_owned(),
        drop_date: DROP,
        pickup: pickup(),
        lines: lines(),
        donation,
        status_url:
            "https://broodvoorweldadigheid.nl/bestelling/0b5e7c2a-1111-4222-8333-944455556666"
                .to_owned(),
    }
}

fn drop_alert(kind: DropAlertKind) -> DropAlert {
    DropAlert {
        to: "abonnee@example.org".to_owned(),
        kind,
        drop_date: DROP,
        pickup: pickup(),
        deadline: Some(ts("2026-12-03T19:00:00Z")),
        charity: Some("Voedselbank Westerkwartier".to_owned()),
        menu: vec!["Desem wit".to_owned(), "Volkoren spelt".to_owned()],
        shop_url: "https://broodvoorweldadigheid.nl/drops/2026-12-05".to_owned(),
        unsubscribe_url: "https://broodvoorweldadigheid.nl/drop-alerts/afmelden/tok3n".to_owned(),
    }
}

fn snapshot(name: &str, email: &Email) {
    insta::assert_snapshot!(
        format!("{name}-text"),
        format!(
            "To: {}\nSubject: {}\n\n{}",
            email.to, email.subject, email.text
        )
    );
    insta::assert_snapshot!(format!("{name}-html"), email.html);
}

fn compose<T>(f: impl FnOnce(&Composer<'_>) -> Result<T, ComposeError>) -> T {
    let i18n = i18n();
    let company = company();
    f(&Composer::new(&i18n, &company)).expect("compose")
}

#[test]
fn order_confirmation_for_a_charity_drop() {
    let email = compose(|c| {
        c.order_confirmation(&confirmation(Donation::Charity {
            name: "Voedselbank Westerkwartier".to_owned(),
        }))
    });
    assert_eq!(email.kind, EmailKind::OrderConfirmation);
    assert_eq!(email.list_unsubscribe, None);
    for part in [&email.text, &email.html] {
        assert!(
            part.contains(
                "https://broodvoorweldadigheid.nl/bestelling/0b5e7c2a-1111-4222-8333-944455556666"
            ),
            "status page link: {part}"
        );
    }
    snapshot("order-confirmation-charity", &email);
}

#[test]
fn order_confirmation_for_a_normal_sale_has_no_donation_text() {
    let email = compose(|c| c.order_confirmation(&confirmation(Donation::NormalSale)));
    for part in [&email.text, &email.html] {
        assert!(!part.contains("goed doel"), "{part}");
        assert!(!part.contains("gaat naar"), "{part}");
    }
    snapshot("order-confirmation-normal-sale", &email);
}

#[test]
fn order_confirmation_with_a_saved_donation_links_to_the_choice() {
    let email = compose(|c| {
        c.order_confirmation(&confirmation(Donation::Saved {
            choose_url: "https://broodvoorweldadigheid.nl/donatie/kies/abc123".to_owned(),
            choose_by: date(2027, 3, 5),
        }))
    });
    assert!(
        email.text.contains("vóór vrijdag 5 maart 2027"),
        "{}",
        email.text
    );
    assert!(
        email
            .html
            .contains("href=\"https://broodvoorweldadigheid.nl/donatie/kies/abc123\"")
    );
    snapshot("order-confirmation-saved", &email);
}

fn on_account() -> OnAccountConfirmation {
    OnAccountConfirmation {
        to: Recipient {
            email: "inkoop@dehoek.example".to_owned(),
            name: Some("Café De Hoek".to_owned()),
        },
        cc: Some("factuur@dehoek.example".to_owned()),
        order_number: "2026-0043".to_owned(),
        drop_date: DROP,
        pickup: pickup(),
        lines: lines()
            .into_iter()
            .map(|line| OrderLine {
                pay_what_you_want: false,
                ..line
            })
            .collect(),
    }
}

#[test]
fn order_on_account_confirmation_asks_no_payment_and_copies_the_invoice_address() {
    let email = compose(|c| c.order_on_account(&on_account()));
    assert_eq!(email.kind, EmailKind::OrderConfirmation);
    assert_eq!(email.to, "inkoop@dehoek.example");
    assert_eq!(email.cc.as_deref(), Some("factuur@dehoek.example"));
    for part in [&email.text, &email.html] {
        assert!(part.contains("op rekening"), "{part}");
        assert!(!part.contains("/bestelling/"), "no status page: {part}");
    }
    insta::assert_snapshot!(
        "order-on-account-text",
        format!(
            "To: {}\nCc: {}\nSubject: {}\n\n{}",
            email.to,
            email.cc.as_deref().unwrap_or_default(),
            email.subject,
            email.text
        )
    );
    insta::assert_snapshot!("order-on-account-html", email.html);

    let raw = mime(email);
    assert!(raw.contains("To: inkoop@dehoek.example\r\n"), "{raw}");
    assert!(raw.contains("Cc: factuur@dehoek.example\r\n"), "{raw}");

    let mut alone = on_account();
    alone.cc = None;
    let email = compose(|c| c.order_on_account(&alone));
    assert_eq!(email.cc, None);
    assert!(!mime(email).contains("Cc:"));

    let i18n = i18n();
    let company = company();
    let composer = Composer::new(&i18n, &company);
    let mut bad = on_account();
    bad.cc = Some("factuur@dehoek.example\r\nBcc: x@y.example".to_owned());
    assert!(matches!(
        composer.order_on_account(&bad),
        Err(ComposeError::Address)
    ));
    let mut empty = on_account();
    empty.lines.clear();
    assert!(matches!(
        composer.order_on_account(&empty),
        Err(ComposeError::NoLines)
    ));
}

#[test]
fn order_confirmation_shows_btw_per_rate_that_adds_up_to_the_lines() {
    let email = compose(|c| c.order_confirmation(&confirmation(Donation::NormalSale)));
    let sold: Vec<Line> = lines()
        .iter()
        .map(|l| Line {
            unit_price: l.unit_price,
            qty: l.qty,
            vat_rate: l.vat_rate,
        })
        .collect();
    let totals = VatTotals::from_lines(&sold).unwrap();
    let nine = totals.rate(VatRate::Nine);
    let twenty_one = totals.rate(VatRate::TwentyOne);
    assert_eq!((nine.net().cents(), nine.vat().cents()), (1858, 167));
    assert_eq!(
        (twenty_one.net().cents(), twenty_one.vat().cents()),
        (1033, 217)
    );
    for expected in [
        format!("Totaal: €{NBSP}32,75"),
        format!("BTW 9% over €{NBSP}18,58: €{NBSP}1,67"),
        format!("BTW 21% over €{NBSP}10,33: €{NBSP}2,17"),
        format!("2 × Desem wit: €{NBSP}13,00 (€{NBSP}6,50 per stuk)"),
        format!("1 × Volkoren spelt: €{NBSP}7,25 (jouw prijs: €{NBSP}7,25 per stuk)"),
    ] {
        assert!(
            email.text.contains(&expected),
            "missing {expected:?} in\n{}",
            email.text
        );
    }
    assert!(email.html.contains("Jouw prijs"));
}

#[test]
fn a_single_rate_order_shows_only_that_rate() {
    let mut order = confirmation(Donation::NormalSale);
    order.lines = vec![line("Desem wit", 100, 1250, VatRate::Nine, false)];
    let email = compose(|c| c.order_confirmation(&order));
    assert!(
        email.text.contains(&format!("Totaal: €{NBSP}1.250,00")),
        "{}",
        email.text
    );
    assert!(email.text.contains("BTW 9%"));
    assert!(!email.text.contains("BTW 21%"));
}

#[test]
fn ophaalmoment_follows_summer_time() {
    let mut order = confirmation(Donation::NormalSale);
    order.drop_date = date(2026, 6, 6);
    order.pickup.starts = ts("2026-06-06T09:00:00Z");
    order.pickup.ends = ts("2026-06-06T10:00:00Z");
    let email = compose(|c| c.order_confirmation(&order));
    assert!(
        email
            .text
            .contains("Ophaalmoment: zaterdag 6 juni 2026, 11:00–12:00"),
        "{}",
        email.text
    );
}

#[test]
fn pickup_reminder() {
    let email = compose(|c| {
        c.pickup_reminder(&PickupReminder {
            to: sanne(),
            order_number: "2026-0042".to_owned(),
            pickup: pickup(),
            items: vec![
                Item {
                    qty: 2,
                    product: "Desem wit".to_owned(),
                },
                Item {
                    qty: 1,
                    product: "Volkoren spelt".to_owned(),
                },
            ],
        })
    });
    assert_eq!(email.kind, EmailKind::PickupReminder);
    snapshot("pickup-reminder", &email);
}

#[test]
fn pickup_moved_names_the_new_moment() {
    let email = compose(|c| {
        c.pickup_moved(&PickupMoved {
            to: sanne(),
            order_number: "2026-0042".to_owned(),
            pickup: Pickup {
                starts: ts("2026-12-05T13:00:00Z"),
                ends: ts("2026-12-05T14:30:00Z"),
                ..pickup()
            },
        })
    });
    assert_eq!(email.kind, EmailKind::PickupMoved);
    assert!(email.text.contains("14:00"), "{}", email.text);
    assert!(email.text.contains("15:30"), "{}", email.text);
    snapshot("pickup-moved", &email);
}

#[test]
fn order_cancelled_with_a_refund() {
    let email = compose(|c| {
        c.order_cancelled(&OrderCancelled {
            to: sanne(),
            order_number: "2026-0042".to_owned(),
            drop_date: DROP,
            refund: Some(Money::from_cents(3275)),
        })
    });
    assert_eq!(email.kind, EmailKind::OrderCancelled);
    snapshot("order-cancelled", &email);
}

#[test]
fn order_cancelled_without_payment_promises_no_refund() {
    let email = compose(|c| {
        c.order_cancelled(&OrderCancelled {
            to: Recipient {
                email: "klant@example.org".to_owned(),
                name: None,
            },
            order_number: "2026-0043".to_owned(),
            drop_date: DROP,
            refund: None,
        })
    });
    assert!(
        email
            .text
            .starts_with("Je bestelling is geannuleerd\n\nHoi,\n"),
        "{}",
        email.text
    );
    assert!(!email.text.contains("terug"), "{}", email.text);
}

#[test]
fn sick_day_cancel_quotes_the_baker_and_names_the_refund() {
    let email = compose(|c| {
        c.sick_day_cancelled(&SickDayCancelled {
            to: sanne(),
            order_number: "2026-0042".to_owned(),
            drop_date: DROP,
            message: "Ik ben ziek, dus de oven blijft koud.\nVolgende week ben ik er weer!\n"
                .to_owned(),
            refund: Some(Money::from_cents(3275)),
        })
    });
    assert_eq!(email.kind, EmailKind::SickDayCancelled);
    assert!(
        email
            .text
            .contains("> Ik ben ziek, dus de oven blijft koud.\n> Volgende week")
    );
    assert!(
        email
            .text
            .contains(&format!("We storten €{NBSP}32,75 terug."))
    );
    snapshot("sick-day-cancelled", &email);
}

#[test]
fn the_bakers_message_is_escaped_in_html() {
    let email = compose(|c| {
        c.sick_day_cancelled(&SickDayCancelled {
            to: sanne(),
            order_number: "1".to_owned(),
            drop_date: DROP,
            message: "<script>alert(1)</script> \" class=\"email-body".to_owned(),
            refund: None,
        })
    });
    assert!(!email.html.contains("<script>"));
    assert!(email.html.contains("&#60;script&#62;") || email.html.contains("&lt;script&gt;"));
    assert!(!email.html.contains(" class=\""), "every class is inlined");
}

#[test]
fn drop_alert_when_the_menu_is_published() {
    let email = compose(|c| c.drop_alert(&drop_alert(DropAlertKind::MenuPublished)));
    assert_eq!(email.kind, EmailKind::DropAlert);
    assert_eq!(
        email.list_unsubscribe.as_deref(),
        Some("https://broodvoorweldadigheid.nl/drop-alerts/afmelden/tok3n")
    );
    assert!(
        email
            .text
            .contains("Afmelden: https://broodvoorweldadigheid.nl/drop-alerts/afmelden/tok3n")
    );
    assert!(
        email
            .text
            .contains("Bestel-deadline: donderdag 3 december 2026, 20:00")
    );
    snapshot("drop-alert-menu", &email);
}

#[test]
fn drop_alert_when_ordering_opens_for_a_normal_sale() {
    let mut alert = drop_alert(DropAlertKind::OrderingOpen);
    alert.charity = None;
    alert.menu.clear();
    alert.deadline = None;
    let email = compose(|c| c.drop_alert(&alert));
    assert!(
        email
            .text
            .contains("Goed doel: Normale verkoop, zonder goed doel")
    );
    assert!(!email.text.contains("Bestel-deadline"));
    snapshot("drop-alert-open", &email);
}

#[test]
fn subscription_confirmation() {
    let email = compose(|c| {
        c.subscription_confirmation(&SubscriptionConfirmation {
            to: "abonnee@example.org".to_owned(),
            confirm_url: "https://broodvoorweldadigheid.nl/drop-alerts/bevestig/tok3n".to_owned(),
        })
    });
    assert_eq!(email.kind, EmailKind::SubscriptionConfirmation);
    assert_eq!(email.list_unsubscribe, None);
    snapshot("subscription-confirmation", &email);
}

#[test]
fn login_link() {
    let email = compose(|c| {
        c.login_link(&LoginLink {
            to: "klant@example.org".to_owned(),
            login_url: "https://broodvoorweldadigheid.nl/inloggen/tok3n".to_owned(),
            valid_minutes: 30,
        })
    });
    assert_eq!(email.kind, EmailKind::LoginLink);
    assert_eq!(email.list_unsubscribe, None);
    assert!(email.text.contains("30 minuten"), "{}", email.text);
    snapshot("login-link", &email);
}

#[test]
fn donation_moved() {
    let email = compose(|c| {
        c.donation_moved(&DonationMoved {
            to: sanne(),
            order_number: "2026-0042".to_owned(),
            amount: Money::from_cents(3275),
            charity: "Hospice Steenwijk".to_owned(),
            drop_date: date(2027, 3, 13),
            status_url:
                "https://broodvoorweldadigheid.nl/bestelling/0b5e7c2a-1111-4222-8333-944455556666"
                    .to_owned(),
        })
    });
    assert_eq!(email.kind, EmailKind::DonationMoved);
    assert!(
        email.subject.contains("Hospice Steenwijk"),
        "{}",
        email.subject
    );
    snapshot("donation-moved", &email);
}

#[test]
fn account_deleted() {
    let email = compose(|c| c.account_deleted("klant@example.org"));
    assert_eq!(email.kind, EmailKind::AccountDeleted);
    assert_eq!(email.list_unsubscribe, None);
    assert!(email.text.contains("7 jaar"), "{}", email.text);
    snapshot("account-deleted", &email);
}

#[test]
fn every_email_carries_the_company_details_and_no_class_attributes() {
    let i18n = i18n();
    let company = company();
    let composer = Composer::new(&i18n, &company);
    let emails = [
        composer.order_confirmation(&confirmation(Donation::NormalSale)),
        composer.drop_alert(&drop_alert(DropAlertKind::OrderingOpen)),
        composer.subscription_confirmation(&SubscriptionConfirmation {
            to: "abonnee@example.org".to_owned(),
            confirm_url: "https://broodvoorweldadigheid.nl/x".to_owned(),
        }),
        composer.login_link(&LoginLink {
            to: "klant@example.org".to_owned(),
            login_url: "https://broodvoorweldadigheid.nl/x".to_owned(),
            valid_minutes: 30,
        }),
        composer.account_deleted("klant@example.org"),
    ];
    for email in emails {
        let email = email.expect("compose");
        for part in [&email.text, &email.html] {
            assert!(part.contains("KvK 12345678"), "{part}");
            assert!(part.contains("8382 CE Frederiksoord"), "{part}");
        }
        assert!(!email.html.contains(" class=\""));
        assert!(email.html.contains(" style=\""));
        assert!(!email.html.contains("var("));
    }
}

#[test]
fn bad_inputs_are_refused() {
    let i18n = i18n();
    let complete = company();
    let c = Composer::new(&i18n, &complete);

    let mut order = confirmation(Donation::NormalSale);
    order.lines.clear();
    assert!(matches!(
        c.order_confirmation(&order),
        Err(ComposeError::NoLines)
    ));

    let mut order = confirmation(Donation::NormalSale);
    order.to.email = "sanne".to_owned();
    assert!(matches!(
        c.order_confirmation(&order),
        Err(ComposeError::Address)
    ));

    let mut order = confirmation(Donation::NormalSale);
    order.lines[0].unit_price = Money::from_cents(i64::MAX);
    assert!(matches!(
        c.order_confirmation(&order),
        Err(ComposeError::Money(_))
    ));

    let mut order = confirmation(Donation::NormalSale);
    order.status_url = "http://broodvoorweldadigheid.nl/bestelling/x".to_owned();
    assert!(matches!(
        c.order_confirmation(&order),
        Err(ComposeError::Url)
    ));

    let mut alert = drop_alert(DropAlertKind::MenuPublished);
    alert.unsubscribe_url = "http://broodvoorweldadigheid.nl/afmelden".to_owned();
    assert!(matches!(c.drop_alert(&alert), Err(ComposeError::Url)));

    let mut reminder = PickupReminder {
        to: sanne(),
        order_number: "1".to_owned(),
        pickup: pickup(),
        items: vec![],
    };
    reminder.pickup.ends = ts("2026-12-05T09:00:00Z");
    assert!(matches!(
        c.pickup_reminder(&reminder),
        Err(ComposeError::I18n(_))
    ));

    for refund in [0, -100] {
        let cancelled = OrderCancelled {
            to: sanne(),
            order_number: "1".to_owned(),
            drop_date: DROP,
            refund: Some(Money::from_cents(refund)),
        };
        assert!(
            matches!(c.order_cancelled(&cancelled), Err(ComposeError::Refund(r)) if r == refund)
        );
    }

    let subscription = SubscriptionConfirmation {
        to: "abonnee@example.org".to_owned(),
        confirm_url: "https://broodvoorweldadigheid.nl/x".to_owned(),
    };
    for blank in ["name", "address", "kvk"] {
        let mut incomplete = company();
        match blank {
            "name" => incomplete.name = " ".to_owned(),
            "address" => incomplete.address.clear(),
            _ => incomplete.kvk.clear(),
        }
        let composer = Composer::new(&i18n, &incomplete);
        assert!(
            matches!(
                composer.subscription_confirmation(&subscription),
                Err(ComposeError::Company)
            ),
            "{blank}"
        );
    }
    let error = c
        .order_confirmation(&OrderConfirmation {
            to: Recipient {
                email: "geheim@".to_owned(),
                name: None,
            },
            ..confirmation(Donation::NormalSale)
        })
        .unwrap_err();
    assert!(
        !error.to_string().contains("geheim"),
        "no customer data in errors"
    );
}

fn mime(email: Email) -> String {
    let from = "Brood voor Weldadigheid <bakker@broodvoorweldadigheid.nl>"
        .parse()
        .unwrap();
    let outgoing = OutgoingEmail {
        id: "0b5e7c2a-1111-4222-8333-944455556666".to_owned(),
        date: ts("2026-12-01T08:00:00Z"),
        email,
    };
    String::from_utf8(
        notify::message(&from, &outgoing)
            .expect("message")
            .formatted(),
    )
    .unwrap()
}

#[test]
fn drop_alerts_carry_one_click_unsubscribe_headers() {
    let raw = mime(compose(|c| {
        c.drop_alert(&drop_alert(DropAlertKind::MenuPublished))
    }));
    assert!(
        raw.contains(
            "List-Unsubscribe: <https://broodvoorweldadigheid.nl/drop-alerts/afmelden/tok3n>\r\n"
        ),
        "{raw}"
    );
    assert!(raw.contains("List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n"));
    assert!(raw.contains(
        "Message-ID: <0b5e7c2a-1111-4222-8333-944455556666@broodvoorweldadigheid.nl>\r\n"
    ));
    assert!(
        raw.contains("Date: Tue, 01 Dec 2026 08:00:00 +0000\r\n"),
        "{raw}"
    );
    assert!(raw.contains("Content-Type: multipart/alternative;"));
    let text = raw
        .find("Content-Type: text/plain; charset=utf-8")
        .expect("text part");
    let html = raw
        .find("Content-Type: text/html; charset=utf-8")
        .expect("html part");
    assert!(text < html, "the text part comes first");
}

#[test]
fn order_emails_have_no_unsubscribe_headers() {
    let raw = mime(compose(|c| {
        c.order_confirmation(&confirmation(Donation::NormalSale))
    }));
    assert!(!raw.contains("List-Unsubscribe"));
    assert!(raw.contains("To: sanne@example.org\r\n"));
    assert!(raw.contains("<bakker@broodvoorweldadigheid.nl>\r\n"));
}

#[test]
fn a_bad_recipient_is_a_permanent_failure() {
    let mut email = compose(|c| c.order_confirmation(&confirmation(Donation::NormalSale)));
    email.to = "geen adres".to_owned();
    let from = "bakker@broodvoorweldadigheid.nl".parse().unwrap();
    let outgoing = OutgoingEmail {
        id: "x".to_owned(),
        date: ts("2026-12-01T08:00:00Z"),
        email,
    };
    assert!(matches!(
        notify::message(&from, &outgoing),
        Err(SendError::Permanent(_))
    ));
}

fn contact() -> ContactMessage {
    ContactMessage {
        to: "bakker@broodvoorweldadigheid.nl".to_owned(),
        name: "Sanne de Vries".to_owned(),
        email: "sanne@example.org".to_owned(),
        message: "Hoi!\nKan ik ook een halfje bestellen?\n\n<b>Groet</b>".to_owned(),
    }
}

#[test]
fn contact_message_goes_to_the_bakery_with_the_sender_as_reply_to() {
    let email = compose(|c| c.contact_message(&contact()));
    assert_eq!(email.kind, EmailKind::ContactMessage);
    assert_eq!(email.to, "bakker@broodvoorweldadigheid.nl");
    assert_eq!(email.reply_to.as_deref(), Some("sanne@example.org"));
    assert_eq!(email.list_unsubscribe, None);
    assert!(email.text.contains("> Kan ik ook een halfje bestellen?"));
    assert!(!email.html.contains("<b>"));
    snapshot("contact-message", &email);

    let raw = mime(email);
    assert!(raw.contains("Reply-To: sanne@example.org\r\n"), "{raw}");
    assert!(
        raw.contains("To: bakker@broodvoorweldadigheid.nl\r\n"),
        "{raw}"
    );
}

#[test]
fn contact_message_refuses_header_injection_and_blanks() {
    let i18n = i18n();
    let company = company();
    let c = Composer::new(&i18n, &company);
    for email in [
        "sanne@example.org\r\nBcc: spam@example.org",
        "sanne@example.org\nBcc: spam@example.org",
        "sanne",
        "",
    ] {
        let message = ContactMessage {
            email: email.to_owned(),
            ..contact()
        };
        assert!(
            matches!(c.contact_message(&message), Err(ComposeError::Address)),
            "{email:?}"
        );
    }
    for name in [
        "Sanne\r\nBcc: spam@example.org",
        "Sanne\n",
        "Sa\u{0}nne",
        " ",
    ] {
        let message = ContactMessage {
            name: name.to_owned(),
            ..contact()
        };
        assert!(
            matches!(c.contact_message(&message), Err(ComposeError::Contact)),
            "{name:?}"
        );
    }
    let message = ContactMessage {
        message: " \n ".to_owned(),
        ..contact()
    };
    assert!(matches!(
        c.contact_message(&message),
        Err(ComposeError::Contact)
    ));
}

#[test]
fn contact_message_needs_only_the_company_name() {
    let i18n = i18n();
    let company = Company {
        address: String::new(),
        kvk: String::new(),
        ..company()
    };
    let email = Composer::new(&i18n, &company)
        .contact_message(&contact())
        .expect("compose");
    assert!(email.text.contains("Brood voor Weldadigheid"));
}

#[test]
fn addresses_are_checked_without_composing() {
    assert!(notify::is_address("sanne@example.org"));
    assert!(!notify::is_address(" sanne@example.org"));
    assert!(!notify::is_address(
        "sanne@example.org\r\nBcc: x@example.org"
    ));
    assert!(!notify::is_address("sanne"));
    assert!(
        !notify::is_address("sanne@[192.0.2.1]"),
        "the SMTP layer refuses domain literals in Reply-To"
    );
}
