use std::sync::atomic::{AtomicU32, Ordering};

use bakedays::{BakeDayProduct, NewBakeDay, Schedule, days};
use domain::{Clock, PickupLocationId, ProductId};
use jiff::{SignedDuration, Timestamp, ToSpan, civil::Date};
use sqlx::{PgConnection, PgPool};

use crate::Error;

struct Now;

impl Clock for Now {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// Differs per call, for names and email addresses that must be new on every run.
#[must_use]
pub fn unique() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("{}{n}", Timestamp::now().as_millisecond())
}

/// A customer address of its own, so the login-link limit per address never applies.
#[must_use]
pub fn customer_email(flow: &str) -> String {
    format!("klant-{flow}-{}@example.org", unique())
}

/// What the back office needs to plan a drop: a pickup location of its own with a
/// window on every weekday, so every date is free, a product with approved allergens, an
/// approved kitchen may-contain list (gluten and nuts), a charity and an owner to sign in
/// as.
#[derive(Debug, Clone)]
pub struct Basics {
    pub location: i64,
    pub product: i64,
    pub product_name: String,
    pub charity: i64,
    pub owner: i64,
}

/// An open charity drop a week from now with one loaf of €4,50 on its menu.
#[derive(Debug, Clone)]
pub struct OpenDrop {
    pub day: i64,
    pub product: i64,
    pub product_name: String,
    pub charity: i64,
    pub owner: i64,
}

/// The date of a new drop: a week ahead, so its bestel-deadline is days away.
///
/// # Errors
/// Only for a date out of range.
pub fn drop_date() -> Result<Date, Error> {
    Ok(Now.now_in_bakery().date().checked_add(7.days())?)
}

/// Adds a charity and returns its id.
///
/// # Errors
/// When the database refuses it.
pub async fn charity(conn: &mut PgConnection, name: &str) -> Result<i64, Error> {
    Ok(sqlx::query_scalar(
        "INSERT INTO charity (name, description, url)
         VALUES ($1, 'Helpt mensen', 'https://example.org/doel') RETURNING id",
    )
    .bind(name)
    .fetch_one(conn)
    .await?)
}

/// Fills in the company address and KvK number; without them no email is composed.
///
/// # Errors
/// When the database refuses it.
pub async fn company(conn: &mut PgConnection) -> Result<(), Error> {
    sqlx::query("UPDATE company SET address = 'Teststraat 1, 1234 AB Testdorp', kvk = '12345678'")
        .execute(conn)
        .await?;
    Ok(())
}

/// Adds a customer, as a first order would, and the company details for the emails
/// they get.
///
/// # Errors
/// When the database refuses it.
pub async fn customer(pool: &PgPool, email: &str) -> Result<i64, Error> {
    company(&mut *pool.acquire().await?).await?;
    Ok(sqlx::query_scalar(
        "INSERT INTO customer (email, name, created_at) VALUES ($1, 'Test Klant', now())
         RETURNING id",
    )
    .bind(email)
    .fetch_one(pool)
    .await?)
}

/// Sets the kitchen-wide may-contain list to gluten and nuts and approves it as `owner`
/// (F10, ADR 0038); without an approved list the shop takes no orders. Gluten is in the
/// seeded loaf, so the shop shows only nuts as a trace. Two statements: changing the list
/// clears its approval.
async fn kitchen_may_contain(conn: &mut PgConnection, owner: i64) -> Result<(), Error> {
    sqlx::query("UPDATE company SET may_contain = ARRAY['cereals_containing_gluten', 'nuts']")
        .execute(&mut *conn)
        .await?;
    sqlx::query("UPDATE company SET may_contain_approved_by = $1, may_contain_approved_at = now()")
        .bind(owner)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Fills a migrated database with what [`Basics`] lists, plus the company details for
/// order emails. Safe to run again: every run adds its own location, product and charity.
///
/// # Errors
/// When the database refuses a step.
pub async fn basics(pool: &PgPool) -> Result<Basics, Error> {
    let mut conn = pool.acquire().await?;
    let tag = unique();
    company(&mut conn).await?;
    let charity = charity(&mut conn, &format!("Testdoel {tag}")).await?;
    let owner: i64 = sqlx::query_scalar(
        "INSERT INTO staff_user (oidc_sub, name, role) VALUES ('e2e-owner', 'Testeigenaar', 'owner')
         ON CONFLICT (oidc_sub) DO UPDATE SET active = true, role = 'owner' RETURNING id",
    )
    .fetch_one(&mut *conn)
    .await?;
    kitchen_may_contain(&mut conn, owner).await?;
    let product_name = format!("Desem {tag}");
    let product: i64 = sqlx::query_scalar(
        "INSERT INTO product (name_nl, description_nl, price_incl_cents, vat_rate,
            pay_what_you_want, unit_weight_g, dough_g_per_piece, sold_online, allergens,
            ingredient_text, allergens_approved_by, allergens_approved_at)
         VALUES ($1, 'Lekker brood', 450, 9, false, 800, 900, true,
            ARRAY['cereals_containing_gluten'], 'Tarwebloem, water, zout', $2, now())
         RETURNING id",
    )
    .bind(&product_name)
    .bind(owner)
    .fetch_one(&mut *conn)
    .await?;
    let location: i64 = sqlx::query_scalar(
        "WITH place AS (INSERT INTO pickup_location (name, address)
            VALUES ($1, 'Teststraat 1, 1234 AB Testdorp') RETURNING id),
         windows AS (INSERT INTO pickup_window (location_id, weekday, starts, ends,
            default_deadline_days_before, default_deadline_time)
            SELECT id, weekday, '11:00', '12:00', 2, '20:00'
            FROM place, generate_series(1, 7) AS weekday)
         SELECT id FROM place",
    )
    .bind(format!("Testlocatie {tag}"))
    .fetch_one(&mut *conn)
    .await?;
    Ok(Basics {
        location,
        product,
        product_name,
        charity,
        owner,
    })
}

/// [`basics`] plus an open drop for its charity with its product on the menu.
///
/// # Errors
/// When the database refuses a step.
pub async fn open_drop(pool: &PgPool) -> Result<OpenDrop, Error> {
    let basics = basics(pool).await?;
    let mut conn = pool.acquire().await?;
    let day = days::create(
        &mut conn,
        &NewBakeDay {
            date: drop_date()?,
            location_id: PickupLocationId::new(basics.location),
            online_ordering: true,
            charity_id: Some(domain::CharityId::new(basics.charity)),
        },
    )
    .await?;
    days::update_schedule(
        &mut conn,
        day.id,
        &Schedule {
            online_ordering: true,
            menu_publish_at: None,
            opens_at: Some(Now.now().checked_sub(SignedDuration::from_hours(1))?),
            cutoff_at: day.cutoff_at,
        },
    )
    .await?;
    days::open(&mut conn, &Now, day.id).await?;
    bakedays::range::set_product(
        &mut conn,
        day.id,
        &BakeDayProduct {
            product_id: ProductId::new(basics.product),
            max_qty: Some(10),
            capacity_pool_id: None,
            units_per_item: 1,
        },
    )
    .await?;
    Ok(OpenDrop {
        day: day.id.get(),
        product: basics.product,
        product_name: basics.product_name,
        charity: basics.charity,
        owner: basics.owner,
    })
}
