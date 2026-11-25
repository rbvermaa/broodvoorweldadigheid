use bakedays::{BakeDayProduct, NewBakeDay, Schedule, days};
use domain::{Clock, PickupLocationId, ProductId};
use jiff::{SignedDuration, Timestamp, ToSpan};
use sqlx::PgPool;

use crate::Error;

const MAX_DAYS_AHEAD: i64 = 60;

struct Now;

impl Clock for Now {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// An open drop a week or more from now with one loaf of €4,50 on its menu.
#[derive(Debug, Clone)]
pub struct OpenDrop {
    pub day: i64,
    pub product: i64,
    pub product_name: String,
}

/// Fills a migrated database with what a customer needs to order: the company details
/// for the confirmation email, a charity, a product with approved allergens and an open
/// drop. Safe to run again: every run adds its own drop.
///
/// # Errors
/// When the database refuses a step.
pub async fn open_drop(pool: &PgPool) -> Result<OpenDrop, Error> {
    let mut conn = pool.acquire().await?;
    sqlx::query("UPDATE company SET address = 'Teststraat 1, 1234 AB Testdorp', kvk = '12345678'")
        .execute(&mut *conn)
        .await?;
    let charity: i64 = sqlx::query_scalar(
        "INSERT INTO charity (name, description, url)
         VALUES ('Testdoel', 'Helpt mensen', 'https://example.org/doel') RETURNING id",
    )
    .fetch_one(&mut *conn)
    .await?;
    let product_name = "Desem".to_owned();
    let product: i64 = sqlx::query_scalar(
        "WITH baker AS (INSERT INTO staff_user (oidc_sub, name, role) VALUES ('e2e-baker', 'Testbakker', 'baker')
            ON CONFLICT (oidc_sub) DO UPDATE SET name = EXCLUDED.name RETURNING id)
         INSERT INTO product (name_nl, description_nl, price_incl_cents, vat_rate,
            pay_what_you_want, unit_weight_g, dough_g_per_piece, sold_online, allergens,
            ingredient_text, allergens_approved_by, allergens_approved_at)
         VALUES ($1, 'Lekker brood', 450, 9, false, 800, 900, true,
            ARRAY['cereals_containing_gluten'], 'Tarwebloem, water, zout',
            (SELECT id FROM baker), now())
         RETURNING id",
    )
    .bind(&product_name)
    .fetch_one(&mut *conn)
    .await?;

    let now = Now.now();
    let first = Now.now_in_bakery().date().checked_add(7.days())?;
    let mut day = None;
    for offset in 0..MAX_DAYS_AHEAD {
        let input = NewBakeDay {
            date: first.checked_add(offset.days())?,
            location_id: PickupLocationId::new(1),
            online_ordering: true,
            charity_id: Some(domain::CharityId::new(charity)),
        };
        match days::create(&mut conn, &input).await {
            Ok(created) => {
                day = Some(created);
                break;
            }
            Err(bakedays::Error::NoPickupWindow(_) | bakedays::Error::DateTaken(_)) => {}
            Err(err) => return Err(err.into()),
        }
    }
    let day = day.ok_or("no free pickup day for the drop in the coming weeks")?;
    days::update_schedule(
        &mut conn,
        day.id,
        &Schedule {
            online_ordering: true,
            menu_publish_at: None,
            opens_at: Some(now.checked_sub(SignedDuration::from_hours(1))?),
            cutoff_at: day.cutoff_at,
        },
    )
    .await?;
    days::open(&mut conn, &Now, day.id).await?;
    bakedays::range::set_product(
        &mut conn,
        day.id,
        &BakeDayProduct {
            product_id: ProductId::new(product),
            max_qty: Some(10),
            capacity_pool_id: None,
            units_per_item: 1,
        },
    )
    .await?;
    Ok(OpenDrop {
        day: day.id.get(),
        product,
        product_name,
    })
}
