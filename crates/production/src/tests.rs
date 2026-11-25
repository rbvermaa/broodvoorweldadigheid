use bakedays::{BakeDay, BakeDayStatus, PickupSlot};
use domain::{BakeDayId, OrderId, PickupLocationId, PickupSlotId, ProductId};
use i18n::Translator;
use orders::{OrderNumber, OrderStatus, PaymentMode};

use crate::needs::Basis;
use crate::{
    Dough, DoughProduct, Extras, IngredientNeeds, Missing, MissingReason, PickupLine, PickupList,
    PickupOrder, ProductTotal, ProductionList, SlotPickups, paper, pdf,
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
        customer_erased: false,
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

fn ingredient_needs() -> IngredientNeeds {
    use domain::IngredientId;
    use recipes::{DoughGrams, IngredientRef, IngredientRole};

    let ingredient = |id: i64, name: &str, role| IngredientRef {
        id: IngredientId::new(id),
        name: name.to_owned(),
        role,
    };
    let flour = ingredient(1, "Tarwebloem", IngredientRole::Flour);
    let water = ingredient(2, "Water", IngredientRole::Water);
    let salt = ingredient(3, "Zeezout", IngredientRole::Other);
    let doughs = vec![
        Dough {
            batch: None,
            recipe: "Desem".to_owned(),
            version: 3,
            products: vec![
                DoughProduct {
                    product_id: ProductId::new(1),
                    name: "Desem".to_owned(),
                    pieces: 12,
                    dough_g_per_piece: 900,
                },
                DoughProduct {
                    product_id: ProductId::new(3),
                    name: "Desem klein".to_owned(),
                    pieces: 2,
                    dough_g_per_piece: 450,
                },
            ],
            grams: DoughGrams {
                ingredients: vec![
                    (flour.clone(), 6_667),
                    (water.clone(), 4_867),
                    (salt.clone(), 166),
                ],
            },
        },
        Dough {
            batch: None,
            recipe: "Spelt".to_owned(),
            version: 1,
            products: vec![DoughProduct {
                product_id: ProductId::new(4),
                name: "Spelt".to_owned(),
                pieces: 1,
                dough_g_per_piece: 500,
            }],
            grams: DoughGrams {
                ingredients: vec![(flour, 290), (water, 210)],
            },
        },
    ];
    IngredientNeeds {
        day: day(),
        basis: Basis::Recipes,
        ingredients: recipes::shopping_list(doughs.iter().map(|d| &d.grams)),
        doughs,
        missing: vec![Missing {
            product_id: ProductId::new(2),
            name: "Granola".to_owned(),
            pieces: 13,
            reason: MissingReason::NoRecipe,
        }],
    }
}

#[test]
fn ingredient_needs_input() {
    let mut data = paper::needs(&ingredient_needs(), &dutch()).unwrap();
    data.sort_all_objects();
    insta::assert_snapshot!(serde_json::to_string_pretty(&data).unwrap());
}

#[test]
fn ingredient_needs_on_paper() {
    let data = paper::needs(&ingredient_needs(), &dutch()).unwrap();
    let (document, warnings) = pdf::layout(&pdf::NEEDS, &data).unwrap();
    assert_eq!(warnings, Vec::<String>::new());
    insta::assert_snapshot!(pdf::text(&document));
}

#[test]
fn empty_ingredient_needs_say_so_and_the_pdf_is_stable() {
    let mut needs = ingredient_needs();
    needs.ingredients.clear();
    needs.doughs.clear();
    needs.missing.clear();
    let data = paper::needs(&needs, &dutch()).unwrap();
    let (document, _) = pdf::layout(&pdf::NEEDS, &data).unwrap();
    let text = pdf::text(&document);
    assert!(text.contains("Nog niets nodig."), "{text}");
    assert!(!text.contains("Geen recept"), "{text}");
    let t = dutch();
    let first = crate::ingredient_needs_pdf(&ingredient_needs(), &t).unwrap();
    assert!(first.starts_with(b"%PDF-"));
    assert_eq!(
        first,
        crate::ingredient_needs_pdf(&ingredient_needs(), &t).unwrap()
    );
}

#[test]
fn needs_after_the_deadline_name_their_batches() {
    let mut needs = ingredient_needs();
    needs.basis = Basis::Batches;
    needs.doughs[0].batch = Some(crate::batches::BatchCode {
        date: jiff::civil::date(2026, 10, 31),
        mix_no: 2,
    });
    let data = paper::needs(&needs, &dutch()).unwrap();
    assert_eq!(
        data["basis"],
        dutch().t("needs-basis-batches", None).unwrap()
    );
    assert_eq!(
        data["doughs"][0]["name"],
        "Batch 261031-2: Desem (versie 3)"
    );
    assert_eq!(data["doughs"][1]["name"], "Spelt (versie 1)");
}

#[test]
fn ingredient_needs_as_csv() {
    let mut needs = ingredient_needs();
    needs.missing[0].name = "Granola; groot".to_owned();
    let csv = crate::ingredient_needs_csv(&needs, &dutch()).unwrap();
    assert_eq!(
        csv,
        "\u{feff}Ingrediënt;Gram\r\n\
         Tarwebloem;6957\r\n\
         Water;5077\r\n\
         Zeezout;166\r\n\
         \"Niet meegeteld (geen recept): Granola; groot, 13 stuks\";\r\n"
    );
}

#[test]
fn needs_with_only_products_without_recipe_list_them_instead_of_nothing() {
    let mut needs = ingredient_needs();
    needs.ingredients.clear();
    needs.doughs.clear();
    let data = paper::needs(&needs, &dutch()).unwrap();
    let (document, _) = pdf::layout(&pdf::NEEDS, &data).unwrap();
    let text = pdf::text(&document);
    assert!(!text.contains("Nog niets nodig."), "{text}");
    assert!(text.contains("Granola | 13 | geen recept"), "{text}");
    assert!(!text.contains("Ingrediënt"), "{text}");
}

#[test]
fn an_erased_customer_is_printed_as_deleted() {
    let mut list = pickup_list();
    let order = &mut list.slots[0].orders[0];
    order.customer_name = "-".to_owned();
    order.customer_phone = None;
    order.customer_erased = true;
    let data = paper::pickup(&list, &dutch()).unwrap();
    let printed = &data["slots"][0]["orders"][0];
    assert_eq!(printed["name"], "verwijderd");
    assert_eq!(printed["erased"], true);
    let (document, warnings) = pdf::layout(&pdf::PICKUP, &data).unwrap();
    assert_eq!(warnings, Vec::<String>::new());
    assert!(pdf::text(&document).contains("verwijderd"));
}

#[test]
fn erased_customers_come_last_in_a_slot() {
    let mut erased = order(1, "-", None, OrderStatus::Paid, &[("Desem", 1)]);
    erased.customer_erased = true;
    let mut orders = vec![
        erased,
        order(2, "Zoë Test", None, OrderStatus::Paid, &[("Desem", 1)]),
        order(3, "anna Test", None, OrderStatus::Paid, &[("Desem", 1)]),
    ];
    crate::pickup::sort_orders(&mut orders);
    let seqs: Vec<i32> = orders.iter().map(|o| o.number.seq).collect();
    assert_eq!(seqs, [3, 2, 1]);
}
