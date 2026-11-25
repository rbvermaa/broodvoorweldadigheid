use bakedays::{BakeDay, BakeDayStatus, PickupSlot};
use domain::{BakeDayId, OrderId, PickupLocationId, PickupSlotId, ProductId};
use i18n::Translator;
use orders::{OrderNumber, OrderStatus, PaymentMode};

use crate::{
    Extras, PickupLine, PickupList, PickupOrder, ProductTotal, ProductionList, SlotPickups, paper,
    pdf,
};

fn day() -> BakeDay {
    BakeDay {
        id: BakeDayId::new(1),
        date: "2026-10-31".parse().unwrap(),
        online_ordering: true,
        charity_id: None,
        menu_publish_at: None,
        opens_at: None,
        cutoff_at: Some("2026-10-29T19:00:00Z".parse().unwrap()),
        bake_starts_at: "2026-10-31T03:00:00Z".parse().unwrap(),
        status: BakeDayStatus::Closed,
        cancel_message: None,
    }
}

fn production_list() -> ProductionList {
    ProductionList {
        day: day(),
        products: vec![
            ProductTotal {
                product_id: ProductId::new(1),
                name: "Desem".to_owned(),
                ordered: 1_204,
                extras: Extras {
                    freezer: 6,
                    sumup: 0,
                    samples: 2,
                    other: 0,
                },
            },
            ProductTotal {
                product_id: ProductId::new(2),
                name: "Granola".to_owned(),
                ordered: 0,
                extras: Extras {
                    sumup: 12,
                    other: 1,
                    ..Extras::default()
                },
            },
        ],
    }
}

fn order(
    seq: i32,
    name: &str,
    phone: Option<&str>,
    status: OrderStatus,
    lines: &[(&str, u32)],
) -> PickupOrder {
    PickupOrder {
        order_id: OrderId::new(i64::from(seq)),
        number: OrderNumber { year: 2026, seq },
        customer_name: name.to_owned(),
        customer_phone: phone.map(str::to_owned),
        status,
        payment_mode: PaymentMode::Online,
        picked_up: status == OrderStatus::PickedUp,
        lines: lines
            .iter()
            .enumerate()
            .map(|(i, (product, qty))| PickupLine {
                product_id: ProductId::new(i64::try_from(i).unwrap() + 1),
                name: (*product).to_owned(),
                qty: *qty,
            })
            .collect(),
    }
}

fn slot(id: i64, starts: &str, ends: &str) -> PickupSlot {
    PickupSlot {
        id: PickupSlotId::new(id),
        bake_day_id: BakeDayId::new(1),
        location_id: PickupLocationId::new(1),
        starts_at: starts.parse().unwrap(),
        ends_at: ends.parse().unwrap(),
        max_orders: None,
    }
}

fn pickup_list() -> PickupList {
    let mut on_account = order(
        9,
        "Bakkerij Test",
        None,
        OrderStatus::Confirmed,
        &[("Desem", 10)],
    );
    on_account.payment_mode = PaymentMode::OnAccount;
    PickupList {
        day: day(),
        slots: vec![
            SlotPickups {
                slot: slot(1, "2026-10-31T10:00:00Z", "2026-10-31T11:00:00Z"),
                location: "De Proef".to_owned(),
                orders: vec![
                    order(
                        3,
                        "Anna Test",
                        Some("0600000000"),
                        OrderStatus::Paid,
                        &[("Desem", 2), ("Koekjes", 1)],
                    ),
                    on_account,
                    order(1, "Piet Test", None, OrderStatus::PickedUp, &[("Desem", 1)]),
                    order(
                        2,
                        "Piet Test",
                        None,
                        OrderStatus::Confirmed,
                        &[("Desem", 1)],
                    ),
                ],
            },
            SlotPickups {
                slot: slot(2, "2026-10-31T11:00:00Z", "2026-10-31T12:00:00Z"),
                location: "De Proef".to_owned(),
                orders: vec![],
            },
        ],
    }
}

fn dutch() -> Translator {
    Translator::dutch().unwrap()
}

#[test]
fn production_list_input() {
    let mut data = paper::production(&production_list(), &dutch()).unwrap();
    // Workspace builds enable serde_json's preserve_order (thirtyfour, in e2e).
    data.sort_all_objects();
    insta::assert_snapshot!(serde_json::to_string_pretty(&data).unwrap());
}

#[test]
fn production_list_on_paper() {
    let data = paper::production(&production_list(), &dutch()).unwrap();
    let (document, warnings) = pdf::layout(&pdf::PRODUCTION, &data).unwrap();
    assert_eq!(warnings, Vec::<String>::new());
    insta::assert_snapshot!(pdf::text(&document));
}

#[test]
fn an_empty_production_list_says_so() {
    let mut list = production_list();
    list.products.clear();
    let data = paper::production(&list, &dutch()).unwrap();
    let (document, _) = pdf::layout(&pdf::PRODUCTION, &data).unwrap();
    let text = pdf::text(&document);
    assert!(text.contains("Nog niets te bakken."), "{text}");
    assert!(!text.contains("Capaciteit"), "{text}");
}

#[test]
fn pickup_list_input() {
    let mut data = paper::pickup(&pickup_list(), &dutch()).unwrap();
    data.sort_all_objects();
    insta::assert_snapshot!(serde_json::to_string_pretty(&data).unwrap());
}

#[test]
fn pickup_list_on_paper() {
    let data = paper::pickup(&pickup_list(), &dutch()).unwrap();
    let (document, warnings) = pdf::layout(&pdf::PICKUP, &data).unwrap();
    assert_eq!(warnings, Vec::<String>::new());
    insta::assert_snapshot!(pdf::text(&document));
}

#[test]
fn a_pending_order_never_reaches_the_paper() {
    let mut list = pickup_list();
    list.slots[0].orders[0].status = OrderStatus::PendingPayment;
    assert!(matches!(
        paper::pickup(&list, &dutch()),
        Err(crate::Error::Corrupt(_))
    ));
}

#[test]
fn the_same_list_gives_the_same_pdf() {
    let t = dutch();
    let first = crate::production_pdf(&production_list(), &t).unwrap();
    assert!(first.starts_with(b"%PDF-"));
    assert_eq!(
        first,
        crate::production_pdf(&production_list(), &t).unwrap()
    );
    let pickup = crate::pickup_pdf(&pickup_list(), &t).unwrap();
    assert!(pickup.starts_with(b"%PDF-"));
    assert_eq!(pickup, crate::pickup_pdf(&pickup_list(), &t).unwrap());
    assert_ne!(first, pickup);
}

#[test]
fn templates_read_no_other_files() {
    let template = pdf::Template::inline("/evil.typ", "#read(\"/etc/passwd\")");
    let Err(crate::Error::Pdf(message)) = pdf::layout(&template, &serde_json::json!({})) else {
        panic!("a template read a file");
    };
    assert!(message.contains("denied"), "{message}");
    let template = pdf::Template::inline("/evil.typ", "#include \"/other.typ\"");
    assert!(pdf::layout(&template, &serde_json::json!({})).is_err());
}

#[test]
fn names_are_printed_as_text_never_as_markup() {
    let mut list = pickup_list();
    let name = "*Bo* #read(\"/a\") $x$ ]";
    list.slots[0].orders[0].customer_name = name.to_owned();
    let data = paper::pickup(&list, &dutch()).unwrap();
    let (document, _) = pdf::layout(&pdf::PICKUP, &data).unwrap();
    let text = pdf::text(&document);
    for literal in ["*Bo*", "#read(\"/a\")", "$x$ ]"] {
        assert!(text.contains(literal), "{text}");
    }
}
