#![forbid(unsafe_code)]

use std::fmt::Write as _;

use bakedays::{
    BakeDayProduct, ExtraInput, NewBakeDay, PickupSlotInput, Purpose, Schedule, days, extras, range,
};
use domain::{
    BakeDayId, CapacityPoolId, IngredientId, PickupLocationId, PickupSlotId, ProductId, RecipeId,
    RecipeVersionId, StaffUserId,
};
use jiff::Timestamp;
use orders::{Cart, CustomerInput, Order, OrderedBy, Orders, PaymentReceived};
use production::{
    IngredientNeeds, PickupList, ProductionList, ingredient_needs, pickup_list, production_list,
};
use recipes::{
    DraftInput, IngredientInput, IngredientRole, LineInput, LinePart, RecipeInput, RecipeKind,
    ingredients, recipes as store,
};
use sqlx::PgConnection;
use sqlx::Postgres;
use sqlx::pool::PoolConnection;
use testkit::FakeClock;

const DE_PROEF: PickupLocationId = PickupLocationId::new(1);
const BAKER: StaffUserId = StaffUserId::new(3);

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

struct Shop {
    day: BakeDayId,
    first: PickupSlotId,
    second: PickupSlotId,
}

/// Saturday 31 October 2026, open on Wednesday, with a second slot 12:00–13:00.
async fn shop(conn: &mut PgConnection, clock: &FakeClock, date: &str) -> Shop {
    let date: jiff::civil::Date = date.parse().unwrap();
    let day = days::create(
        conn,
        &NewBakeDay {
            date,
            location_id: DE_PROEF,
            online_ordering: true,
            charity_id: None,
        },
    )
    .await
    .unwrap();
    days::update_schedule(
        conn,
        day.id,
        &Schedule {
            online_ordering: true,
            menu_publish_at: None,
            opens_at: Some(ts("2026-10-01T00:00:00Z")),
            cutoff_at: day.cutoff_at,
        },
    )
    .await
    .unwrap();
    days::open(conn, clock, day.id).await.unwrap();
    let first = days::list_slots(conn, day.id).await.unwrap()[0];
    let second = days::add_slot(
        conn,
        day.id,
        DE_PROEF,
        &PickupSlotInput {
            starts_at: first.ends_at,
            ends_at: first.ends_at + jiff::SignedDuration::from_hours(1),
            max_orders: None,
        },
    )
    .await
    .unwrap();
    Shop {
        day: day.id,
        first: first.id,
        second,
    }
}

async fn product(conn: &mut PgConnection, name: &str, sold_online: bool) -> ProductId {
    let id: i64 = sqlx::query_scalar(
        "WITH baker AS (INSERT INTO staff_user (oidc_sub, name, role) VALUES ('test-baker', 'Testbakker', 'baker')
            ON CONFLICT (oidc_sub) DO UPDATE SET name = EXCLUDED.name RETURNING id),
         kitchen AS (UPDATE company SET may_contain_approved_by = (SELECT id FROM baker),
            may_contain_approved_at = '2026-08-01T10:00:00Z')
         INSERT INTO product (name_nl, price_incl_cents, vat_rate, unit_weight_g,
            dough_g_per_piece, sold_online, allergens_approved_by, allergens_approved_at)
         VALUES ($1, 450, 9, 800, 900, $2, (SELECT id FROM baker), '2026-10-01T10:00:00Z') RETURNING id",
    )
    .bind(name)
    .bind(sold_online)
    .fetch_one(conn)
    .await
    .unwrap();
    ProductId::new(id)
}

async fn on_range(
    conn: &mut PgConnection,
    day: BakeDayId,
    product_id: ProductId,
    pool: Option<CapacityPoolId>,
    units_per_item: u32,
) {
    range::set_product(
        conn,
        day,
        &BakeDayProduct {
            product_id,
            max_qty: None,
            capacity_pool_id: pool,
            units_per_item,
        },
    )
    .await
    .unwrap();
}

async fn order(
    conn: &mut PgConnection,
    clock: &FakeClock,
    shop: &Shop,
    slot: PickupSlotId,
    name: &str,
    phone: Option<&str>,
    lines: &[(ProductId, u32)],
) -> Order {
    let mut cart = Cart::new(shop.day);
    for &(product, qty) in lines {
        cart.set(product, qty, None).unwrap();
    }
    let customer = CustomerInput {
        email: format!("{}@example.org", name.to_lowercase().replace(' ', ".")),
        name: name.to_owned(),
        phone: phone.map(str::to_owned),
        marketing_opt_in: false,
    };
    orders::checkout(conn, clock, &cart, &customer, slot, OrderedBy::Guest)
        .await
        .unwrap()
}

async fn paid(
    conn: &mut PgConnection,
    clock: &FakeClock,
    shop: &Shop,
    slot: PickupSlotId,
    name: &str,
    phone: Option<&str>,
    lines: &[(ProductId, u32)],
) -> Order {
    let order = order(conn, clock, shop, slot, name, phone, lines).await;
    let payment = PaymentReceived {
        reference: format!("tr_{}", order.id.get()),
        amount: order.total,
    };
    orders::mark_paid(conn, clock, order.id, &payment)
        .await
        .unwrap();
    order
}

async fn extra(
    conn: &mut PgConnection,
    clock: &FakeClock,
    shop: &Shop,
    product: ProductId,
    qty: u32,
    purpose: Purpose,
) {
    extras::add(
        conn,
        shop.day,
        product,
        &ExtraInput {
            qty,
            purpose,
            note: None,
        },
        &Orders::new(clock),
    )
    .await
    .unwrap();
}

fn production_table(list: &ProductionList) -> String {
    let mut out = format!("{}\n", list.day.date);
    for p in &list.products {
        let e = p.extras;
        writeln!(
            out,
            "{}: ordered {} freezer {} sumup {} samples {} other {} -> bake {}",
            p.name,
            p.ordered,
            e.freezer,
            e.sumup,
            e.samples,
            e.other,
            p.to_bake()
        )
        .unwrap();
    }
    out
}

fn pickup_table(list: &PickupList) -> String {
    let mut out = format!("{}\n", list.day.date);
    for slot in &list.slots {
        writeln!(
            out,
            "{} – {} {}",
            slot.slot.starts_at, slot.slot.ends_at, slot.location
        )
        .unwrap();
        for o in &slot.orders {
            let lines: Vec<String> = o
                .lines
                .iter()
                .map(|l| format!("{}x {}", l.qty, l.name))
                .collect();
            writeln!(
                out,
                "  [{}] {} {} {} {} {}: {}",
                if o.picked_up { "x" } else { " " },
                o.number,
                o.customer_name,
                o.customer_phone.as_deref().unwrap_or("-"),
                o.status,
                o.payment_mode,
                lines.join(", ")
            )
            .unwrap();
        }
    }
    out
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_production_list_counts_firm_orders_and_extras(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-28T12:00:00Z"));
    let shop = shop(&mut conn, &clock, "2026-10-31").await;
    let other_day = self::shop(&mut conn, &clock, "2026-11-07").await;
    let oven = range::create_pool(&mut conn, shop.day, "Oven", 100)
        .await
        .unwrap();
    range::create_pool(&mut conn, shop.day, "Koelkast", 20)
        .await
        .unwrap();
    let desem = product(&mut conn, "Desem", true).await;
    let koekjes = product(&mut conn, "koekjes", true).await;
    let spelt = product(&mut conn, "Spelt", true).await;
    let granola = product(&mut conn, "Granola", false).await;
    on_range(&mut conn, shop.day, desem, Some(oven.id), 2).await;
    on_range(&mut conn, shop.day, koekjes, Some(oven.id), 1).await;
    on_range(&mut conn, shop.day, spelt, None, 1).await;
    on_range(&mut conn, other_day.day, desem, None, 1).await;

    paid(
        &mut conn,
        &clock,
        &shop,
        shop.first,
        "Anna Test",
        None,
        &[(desem, 2), (koekjes, 3)],
    )
    .await;
    let picked = paid(
        &mut conn,
        &clock,
        &shop,
        shop.second,
        "Bob Test",
        None,
        &[(desem, 1)],
    )
    .await;
    orders::mark_picked_up(&mut conn, &clock, picked.id, BAKER)
        .await
        .unwrap();
    order(
        &mut conn,
        &clock,
        &shop,
        shop.first,
        "Held Test",
        None,
        &[(desem, 5), (spelt, 1)],
    )
    .await;
    paid(
        &mut conn,
        &clock,
        &other_day,
        other_day.first,
        "Later Test",
        None,
        &[(desem, 7)],
    )
    .await;
    extra(&mut conn, &clock, &shop, desem, 4, Purpose::Freezer).await;
    extra(&mut conn, &clock, &shop, desem, 2, Purpose::Samples).await;
    extra(&mut conn, &clock, &shop, desem, 1, Purpose::Samples).await;
    extra(&mut conn, &clock, &shop, granola, 12, Purpose::SumUp).await;

    let list = production_list(&mut conn, shop.day).await.unwrap();
    insta::assert_snapshot!(production_table(&list), @r"
    2026-10-31
    Desem: ordered 3 freezer 4 sumup 0 samples 3 other 0 -> bake 10
    Granola: ordered 0 freezer 0 sumup 12 samples 0 other 0 -> bake 12
    koekjes: ordered 3 freezer 0 sumup 0 samples 0 other 0 -> bake 3
    ");

    clock.advance(orders::HOLD);
    orders::release_expired_holds(&mut conn, &clock)
        .await
        .unwrap();
    assert_eq!(production_list(&mut conn, shop.day).await.unwrap(), list);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn an_empty_day_has_an_empty_production_list(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-28T12:00:00Z"));
    let shop = shop(&mut conn, &clock, "2026-10-31").await;
    let list = production_list(&mut conn, shop.day).await.unwrap();
    assert!(list.products.is_empty());
    assert!(matches!(
        production_list(&mut conn, BakeDayId::new(999_999)).await,
        Err(production::Error::BakeDays(bakedays::Error::NotFound))
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_pickup_list_groups_by_slot_and_sorts_by_name(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-28T12:00:00Z"));
    let shop = shop(&mut conn, &clock, "2026-10-31").await;
    let desem = product(&mut conn, "Desem", true).await;
    let koekjes = product(&mut conn, "Koekjes", true).await;
    on_range(&mut conn, shop.day, desem, None, 1).await;
    on_range(&mut conn, shop.day, koekjes, None, 1).await;

    paid(
        &mut conn,
        &clock,
        &shop,
        shop.first,
        "Piet Test",
        None,
        &[(desem, 1)],
    )
    .await;
    paid(
        &mut conn,
        &clock,
        &shop,
        shop.first,
        "anna Test",
        Some("0600000000"),
        &[(desem, 2), (koekjes, 1)],
    )
    .await;
    let picked = paid(
        &mut conn,
        &clock,
        &shop,
        shop.first,
        "Piet Test",
        None,
        &[(koekjes, 2)],
    )
    .await;
    orders::mark_picked_up(&mut conn, &clock, picked.id, BAKER)
        .await
        .unwrap();
    order(
        &mut conn,
        &clock,
        &shop,
        shop.first,
        "Held Test",
        None,
        &[(desem, 1)],
    )
    .await;
    paid(
        &mut conn,
        &clock,
        &shop,
        shop.second,
        "Zoë Test",
        None,
        &[(desem, 1)],
    )
    .await;
    let late = order(
        &mut conn,
        &clock,
        &shop,
        shop.second,
        "Late Test",
        None,
        &[(desem, 1)],
    )
    .await;
    clock.advance(orders::HOLD);
    orders::release_expired_holds(&mut conn, &clock)
        .await
        .unwrap();
    assert_eq!(
        orders::order::get(&mut conn, late.id).await.unwrap().status,
        orders::OrderStatus::Cancelled
    );

    let list = pickup_list(&mut conn, shop.day).await.unwrap();
    insta::assert_snapshot!(pickup_table(&list), @r"
    2026-10-31
    2026-10-31T10:00:00Z – 2026-10-31T11:00:00Z De Proef
      [ ] 2026-0002 anna Test 0600000000 paid online: 2x Desem, 1x Koekjes
      [ ] 2026-0001 Piet Test - paid online: 1x Desem
      [x] 2026-0003 Piet Test - picked_up online: 2x Koekjes
    2026-10-31T11:00:00Z – 2026-10-31T12:00:00Z De Proef
      [ ] 2026-0005 Zoë Test - paid online: 1x Desem
    ");

    let undone = orders::undo_picked_up(&mut conn, &clock, picked.id, BAKER)
        .await
        .unwrap();
    let list = pickup_list(&mut conn, shop.day).await.unwrap();
    let ticked = &list.slots[0].orders[2];
    assert_eq!(ticked.order_id, undone.id);
    assert!(!ticked.picked_up);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_day_without_orders_still_lists_its_slots(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-28T12:00:00Z"));
    let shop = shop(&mut conn, &clock, "2026-10-31").await;
    let list = pickup_list(&mut conn, shop.day).await.unwrap();
    assert_eq!(list.slots.len(), 2);
    assert!(list.slots.iter().all(|s| s.orders.is_empty()));
    assert!(matches!(
        pickup_list(&mut conn, BakeDayId::new(999_999)).await,
        Err(production::Error::BakeDays(bakedays::Error::NotFound))
    ));
}

fn pct(percent: u32) -> recipes::Pct {
    recipes::Pct::from_thousandths(percent * 1000)
}

async fn ingredient(conn: &mut PgConnection, name: &str, role: IngredientRole) -> IngredientId {
    ingredients::create(
        conn,
        &IngredientInput {
            name: name.to_owned(),
            label_name: String::new(),
            role,
            supplier_name: String::new(),
            allergens: std::collections::BTreeSet::new(),
        },
    )
    .await
    .unwrap()
    .id
}

/// A recipe whose version 1 has these lines and is in use.
async fn recipe(
    conn: &mut PgConnection,
    name: &str,
    kind: RecipeKind,
    lines: &[(LinePart, u32)],
) -> (RecipeId, RecipeVersionId) {
    let (recipe, version) = store::create(
        conn,
        &RecipeInput {
            name: name.to_owned(),
            kind,
        },
    )
    .await
    .unwrap();
    store::save_draft(
        conn,
        version.id,
        &DraftInput {
            lines: lines
                .iter()
                .map(|&(part, percent)| LineInput {
                    pct: pct(percent),
                    part,
                })
                .collect(),
            lead_time_hours: None,
            notes: String::new(),
        },
    )
    .await
    .unwrap();
    (recipe.id, version.id)
}

async fn dough_weight(conn: &mut PgConnection, product: ProductId, grams: i32) {
    sqlx::query("UPDATE product SET dough_g_per_piece = $2 WHERE id = $1")
        .bind(product.get())
        .bind(grams)
        .execute(conn)
        .await
        .unwrap();
}

fn needs_table(needs: &IngredientNeeds) -> String {
    let mut out = format!("{}\n", needs.day.date);
    for need in &needs.ingredients {
        writeln!(
            out,
            "{}: {} g {:?}",
            need.ingredient.name, need.grams, need.parts
        )
        .unwrap();
    }
    for dough in &needs.doughs {
        let products: Vec<String> = dough
            .products
            .iter()
            .map(|p| format!("{}x {} à {} g", p.pieces, p.name, p.dough_g_per_piece))
            .collect();
        writeln!(
            out,
            "dough {} v{} {} g: {}",
            dough.recipe,
            dough.version,
            dough.grams.total(),
            products.join(", ")
        )
        .unwrap();
    }
    for missing in &needs.missing {
        writeln!(
            out,
            "missing {} {}x {:?}",
            missing.name, missing.pieces, missing.reason
        )
        .unwrap();
    }
    out
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn ingredient_needs_scale_the_recipe_in_use_for_orders_and_extras(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = FakeClock::new(ts("2026-10-28T12:00:00Z"));
    let shop = shop(&mut conn, &clock, "2026-10-31").await;
    let flour = ingredient(&mut conn, "Tarwebloem", IngredientRole::Flour).await;
    let rye = ingredient(&mut conn, "Roggemeel", IngredientRole::Flour).await;
    let water = ingredient(&mut conn, "Water", IngredientRole::Water).await;
    let salt = ingredient(&mut conn, "Zeezout", IngredientRole::Other).await;
    let (_, levain) = recipe(
        &mut conn,
        "Levain",
        RecipeKind::Preferment,
        &[
            (LinePart::Ingredient(rye), 100),
            (LinePart::Ingredient(water), 100),
        ],
    )
    .await;
    let (desem_recipe, _) = recipe(
        &mut conn,
        "Desem",
        RecipeKind::Dough,
        &[
            (LinePart::Ingredient(flour), 90),
            (LinePart::Ingredient(water), 65),
            (LinePart::Ingredient(salt), 2),
            (LinePart::Sub(levain), 20),
        ],
    )
    .await;
    let (empty_recipe, _) = store::create(
        &mut conn,
        &RecipeInput {
            name: "Leeg".to_owned(),
            kind: RecipeKind::Dough,
        },
    )
    .await
    .map(|(recipe, version)| (recipe.id, version.id))
    .unwrap();
    let (unused_recipe, _) = recipe(
        &mut conn,
        "Zonder versie",
        RecipeKind::Dough,
        &[(LinePart::Ingredient(flour), 100)],
    )
    .await;
    sqlx::query("UPDATE recipe SET current_version_id = NULL WHERE id = $1")
        .bind(unused_recipe.get())
        .execute(&mut *conn)
        .await
        .unwrap();

    let desem = product(&mut conn, "Desem", true).await;
    let klein = product(&mut conn, "Desem klein", true).await;
    let granola = product(&mut conn, "Granola", false).await;
    let spelt = product(&mut conn, "Spelt", true).await;
    let rogge = product(&mut conn, "Rogge", true).await;
    let koekjes = product(&mut conn, "Koekjes", true).await;
    dough_weight(&mut conn, klein, 450).await;
    dough_weight(&mut conn, koekjes, 0).await;
    for (product, recipe_id) in [
        (desem, desem_recipe),
        (klein, desem_recipe),
        (koekjes, desem_recipe),
        (spelt, empty_recipe),
        (rogge, unused_recipe),
    ] {
        store::link_product(&mut conn, recipe_id, product)
            .await
            .unwrap();
    }
    for product in [desem, spelt, rogge, koekjes] {
        on_range(&mut conn, shop.day, product, None, 1).await;
    }

    paid(
        &mut conn,
        &clock,
        &shop,
        shop.first,
        "Anna Test",
        None,
        &[(desem, 2), (spelt, 1), (koekjes, 3)],
    )
    .await;
    order(
        &mut conn,
        &clock,
        &shop,
        shop.first,
        "Held Test",
        None,
        &[(desem, 5), (rogge, 1)],
    )
    .await;
    extra(&mut conn, &clock, &shop, desem, 4, Purpose::Freezer).await;
    extra(&mut conn, &clock, &shop, klein, 2, Purpose::Samples).await;
    extra(&mut conn, &clock, &shop, granola, 12, Purpose::SumUp).await;
    extra(&mut conn, &clock, &shop, rogge, 1, Purpose::Other).await;

    let needs = ingredient_needs(&mut conn, shop.day).await.unwrap();
    // Desem: 6 × 900 g + 2 × 450 g = 6300 g at 177%; the levain line gets 712 g.
    insta::assert_snapshot!(needs_table(&needs), @r"
    2026-10-31
    Roggemeel: 356 g [(0, 356)]
    Tarwebloem: 3203 g [(0, 3203)]
    Water: 2670 g [(0, 2670)]
    Zeezout: 71 g [(0, 71)]
    dough Desem v1 6300 g: 6x Desem à 900 g, 2x Desem klein à 450 g
    missing Granola 12x NoRecipe
    missing Koekjes 3x NoDoughWeight
    missing Rogge 1x NoVersion
    missing Spelt 1x EmptyRecipe
    ");
    let total: u64 = needs.ingredients.iter().map(|n| n.grams).sum();
    assert_eq!(total, 6300);
    assert!(matches!(
        ingredient_needs(&mut conn, BakeDayId::new(999_999)).await,
        Err(production::Error::BakeDays(bakedays::Error::NotFound))
    ));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn ingredient_needs_follow_the_version_in_use(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-10-28T12:00:00Z"));
    let shop = shop(&mut conn, &clock, "2026-10-31").await;
    let flour = ingredient(&mut conn, "Tarwebloem", IngredientRole::Flour).await;
    let water = ingredient(&mut conn, "Water", IngredientRole::Water).await;
    let (recipe_id, first) = recipe(
        &mut conn,
        "Wit",
        RecipeKind::Dough,
        &[
            (LinePart::Ingredient(flour), 100),
            (LinePart::Ingredient(water), 80),
        ],
    )
    .await;
    store::freeze(&mut conn, recipe_id, first, &clock)
        .await
        .unwrap();
    let second = store::new_version(&mut conn, recipe_id).await.unwrap();
    store::save_draft(
        &mut conn,
        second.id,
        &DraftInput {
            lines: vec![
                LineInput {
                    pct: pct(100),
                    part: LinePart::Ingredient(flour),
                },
                LineInput {
                    pct: pct(50),
                    part: LinePart::Ingredient(water),
                },
            ],
            lead_time_hours: None,
            notes: String::new(),
        },
    )
    .await
    .unwrap();
    let wit = product(&mut conn, "Wit", true).await;
    store::link_product(&mut conn, recipe_id, wit)
        .await
        .unwrap();
    extra(&mut conn, &clock, &shop, wit, 2, Purpose::Freezer).await;

    let needs = ingredient_needs(&mut conn, shop.day).await.unwrap();
    assert_eq!(needs.doughs[0].version, 1);
    assert_eq!(needs.ingredients[1].grams, 800);

    store::set_current(&mut conn, recipe_id, second.id)
        .await
        .unwrap();
    let needs = ingredient_needs(&mut conn, shop.day).await.unwrap();
    assert_eq!(needs.doughs[0].version, 2);
    assert_eq!(
        needs
            .ingredients
            .iter()
            .map(|n| (n.ingredient.name.as_str(), n.grams))
            .collect::<Vec<_>>(),
        vec![("Tarwebloem", 1200), ("Water", 600)]
    );
}
