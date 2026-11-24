use std::collections::BTreeSet;

use domain::{Allergen, Clock, Money, PayWhatYouWant, ProductId, StaffUserId, VatRate};
use jiff::Timestamp;
use sqlx::PgConnection;

use crate::Error;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AllergenInfo {
    pub allergens: BTreeSet<Allergen>,
    pub may_contain: BTreeSet<Allergen>,
    pub ingredient_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Approval {
    pub by: StaffUserId,
    pub at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pricing {
    Fixed(Money),
    PayWhatYouWant(PayWhatYouWant),
}

impl Pricing {
    /// The fixed price, or the suggested price of a pay-what-you-want product.
    #[must_use]
    pub const fn price_incl(&self) -> Money {
        match self {
            Self::Fixed(price) => *price,
            Self::PayWhatYouWant(pwyw) => pwyw.suggested(),
        }
    }
}

/// The editable fields of a product. For a pay-what-you-want product `price_incl` is the
/// suggested price and `pay_what_you_want` holds the minimum and maximum (F4b).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductInput {
    pub name_nl: String,
    pub description_nl: String,
    pub photo_path: Option<String>,
    pub price_incl: Money,
    pub vat_rate: VatRate,
    pub pay_what_you_want: Option<(Money, Money)>,
    pub unit_weight_g: i32,
    pub dough_g_per_piece: i32,
    pub freezer_days: Option<i32>,
    pub lead_time_hours: Option<i32>,
    pub allergen_info: AllergenInfo,
    pub packaged: bool,
    pub sold_online: bool,
    pub estimated_cost: Option<Money>,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Product {
    pub id: ProductId,
    pub name_nl: String,
    pub description_nl: String,
    pub photo_path: Option<String>,
    pub pricing: Pricing,
    pub vat_rate: VatRate,
    pub unit_weight_g: i32,
    pub dough_g_per_piece: i32,
    pub freezer_days: Option<i32>,
    pub lead_time_hours: Option<i32>,
    pub allergen_info: AllergenInfo,
    pub approval: Option<Approval>,
    pub packaged: bool,
    pub sold_online: bool,
    pub estimated_cost: Option<Money>,
    pub active: bool,
}

impl Product {
    /// Allergen data that may be shown to customers: only once approved.
    #[must_use]
    pub const fn published_allergen_info(&self) -> Option<&AllergenInfo> {
        match self.approval {
            Some(_) => Some(&self.allergen_info),
            None => None,
        }
    }
}

struct Row {
    id: i64,
    name_nl: String,
    description_nl: String,
    photo_path: Option<String>,
    price_incl_cents: i64,
    vat_rate: i16,
    pay_what_you_want: bool,
    min_price_cents: Option<i64>,
    max_price_cents: Option<i64>,
    unit_weight_g: i32,
    dough_g_per_piece: i32,
    freezer_days: Option<i32>,
    lead_time_hours: Option<i32>,
    allergens: Vec<String>,
    may_contain: Vec<String>,
    ingredient_text: String,
    allergens_approved_by: Option<i64>,
    allergens_approved_at_us: Option<i64>,
    packaged: bool,
    sold_online: bool,
    estimated_cost_cents: Option<i64>,
    active: bool,
}

fn allergens_from_db(codes: &[String]) -> Result<BTreeSet<Allergen>, Error> {
    codes
        .iter()
        .map(|code| code.parse().map_err(|e| Error::Corrupt(format!("{e}"))))
        .collect()
}

fn allergens_to_db(allergens: &BTreeSet<Allergen>) -> Vec<String> {
    allergens.iter().map(|a| a.code().to_owned()).collect()
}

impl TryFrom<Row> for Product {
    type Error = Error;

    fn try_from(row: Row) -> Result<Self, Error> {
        let price = Money::from_cents(row.price_incl_cents);
        let pricing = match (
            row.pay_what_you_want,
            row.min_price_cents,
            row.max_price_cents,
        ) {
            (false, _, _) => Pricing::Fixed(price),
            (true, Some(min), Some(max)) => Pricing::PayWhatYouWant(
                PayWhatYouWant::new(price, Money::from_cents(min), Money::from_cents(max))
                    .map_err(|e| Error::Corrupt(format!("product {}: {e}", row.id)))?,
            ),
            (true, _, _) => {
                return Err(Error::Corrupt(format!(
                    "product {}: pay what you want without bounds",
                    row.id
                )));
            }
        };
        let approval = match (row.allergens_approved_by, row.allergens_approved_at_us) {
            (Some(by), Some(at)) => Some(Approval {
                by: StaffUserId::new(by),
                at: db::time::from_micros(at)?,
            }),
            _ => None,
        };
        Ok(Self {
            id: ProductId::new(row.id),
            name_nl: row.name_nl,
            description_nl: row.description_nl,
            photo_path: row.photo_path,
            pricing,
            vat_rate: VatRate::try_from(row.vat_rate).map_err(|e| Error::Corrupt(e.to_string()))?,
            unit_weight_g: row.unit_weight_g,
            dough_g_per_piece: row.dough_g_per_piece,
            freezer_days: row.freezer_days,
            lead_time_hours: row.lead_time_hours,
            allergen_info: AllergenInfo {
                allergens: allergens_from_db(&row.allergens)?,
                may_contain: allergens_from_db(&row.may_contain)?,
                ingredient_text: row.ingredient_text,
            },
            approval,
            packaged: row.packaged,
            sold_online: row.sold_online,
            estimated_cost: row.estimated_cost_cents.map(Money::from_cents),
            active: row.active,
        })
    }
}

struct Validated {
    min_price_cents: Option<i64>,
    max_price_cents: Option<i64>,
    allergens: Vec<String>,
    may_contain: Vec<String>,
}

fn validate(input: &ProductInput) -> Result<Validated, Error> {
    if input.name_nl.trim().is_empty() {
        return Err(Error::Invalid("name"));
    }
    if input.price_incl < Money::ZERO {
        return Err(Error::Invalid("price"));
    }
    if input.unit_weight_g <= 0 {
        return Err(Error::Invalid("unit weight"));
    }
    if input.dough_g_per_piece < 0 {
        return Err(Error::Invalid("dough weight"));
    }
    if input.freezer_days.is_some_and(|days| days <= 0) {
        return Err(Error::Invalid("freezer days"));
    }
    if input
        .lead_time_hours
        .is_some_and(|hours| !(0..=168).contains(&hours))
    {
        return Err(Error::Invalid("lead time"));
    }
    if input.estimated_cost.is_some_and(|cost| cost < Money::ZERO) {
        return Err(Error::Invalid("estimated cost"));
    }
    let (min_price_cents, max_price_cents) = match input.pay_what_you_want {
        Some((min, max)) => {
            let pwyw = PayWhatYouWant::new(input.price_incl, min, max)?;
            (Some(pwyw.min().cents()), Some(pwyw.max().cents()))
        }
        None => (None, None),
    };
    Ok(Validated {
        min_price_cents,
        max_price_cents,
        allergens: allergens_to_db(&input.allergen_info.allergens),
        may_contain: allergens_to_db(&input.allergen_info.may_contain),
    })
}

/// # Errors
/// [`Error::Invalid`] or [`Error::Price`] when the input is refused.
pub async fn create(conn: &mut PgConnection, input: &ProductInput) -> Result<Product, Error> {
    let v = validate(input)?;
    let row = sqlx::query_as!(
        Row,
        r#"INSERT INTO product (
            name_nl, description_nl, photo_path, price_incl_cents, vat_rate,
            pay_what_you_want, min_price_cents, max_price_cents, unit_weight_g,
            dough_g_per_piece, freezer_days, lead_time_hours, allergens, may_contain,
            ingredient_text, packaged, sold_online, estimated_cost_cents, active
        ) VALUES ($1, $2, $3, $4, $5::bigint, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)
        RETURNING id, name_nl, description_nl, photo_path, price_incl_cents, vat_rate,
            pay_what_you_want, min_price_cents, max_price_cents, unit_weight_g,
            dough_g_per_piece, freezer_days, lead_time_hours, allergens, may_contain,
            ingredient_text, allergens_approved_by,
            (extract(epoch FROM allergens_approved_at) * 1000000)::bigint AS allergens_approved_at_us,
            packaged, sold_online, estimated_cost_cents, active"#,
        input.name_nl.trim(),
        input.description_nl,
        input.photo_path,
        input.price_incl.cents(),
        input.vat_rate.percent(),
        input.pay_what_you_want.is_some(),
        v.min_price_cents,
        v.max_price_cents,
        input.unit_weight_g,
        input.dough_g_per_piece,
        input.freezer_days,
        input.lead_time_hours,
        &v.allergens,
        &v.may_contain,
        input.allergen_info.ingredient_text,
        input.packaged,
        input.sold_online,
        input.estimated_cost.map(Money::cents),
        input.active,
    )
    .fetch_one(conn)
    .await?;
    row.try_into()
}

/// Replaces every editable field. A change to the allergens, may-contain list or
/// ingredient text clears the allergen approval (database trigger).
///
/// # Errors
/// [`Error::NotFound`], or [`Error::Invalid`] / [`Error::Price`] when the input is refused.
pub async fn update(
    conn: &mut PgConnection,
    id: ProductId,
    input: &ProductInput,
) -> Result<Product, Error> {
    let v = validate(input)?;
    let row = sqlx::query_as!(
        Row,
        r#"UPDATE product SET
            name_nl = $2, description_nl = $3, photo_path = $4, price_incl_cents = $5,
            vat_rate = $6::bigint, pay_what_you_want = $7, min_price_cents = $8, max_price_cents = $9,
            unit_weight_g = $10, dough_g_per_piece = $11, freezer_days = $12,
            lead_time_hours = $13, allergens = $14, may_contain = $15, ingredient_text = $16,
            packaged = $17, sold_online = $18, estimated_cost_cents = $19, active = $20
        WHERE id = $1
        RETURNING id, name_nl, description_nl, photo_path, price_incl_cents, vat_rate,
            pay_what_you_want, min_price_cents, max_price_cents, unit_weight_g,
            dough_g_per_piece, freezer_days, lead_time_hours, allergens, may_contain,
            ingredient_text, allergens_approved_by,
            (extract(epoch FROM allergens_approved_at) * 1000000)::bigint AS allergens_approved_at_us,
            packaged, sold_online, estimated_cost_cents, active"#,
        id.get(),
        input.name_nl.trim(),
        input.description_nl,
        input.photo_path,
        input.price_incl.cents(),
        input.vat_rate.percent(),
        input.pay_what_you_want.is_some(),
        v.min_price_cents,
        v.max_price_cents,
        input.unit_weight_g,
        input.dough_g_per_piece,
        input.freezer_days,
        input.lead_time_hours,
        &v.allergens,
        &v.may_contain,
        input.allergen_info.ingredient_text,
        input.packaged,
        input.sold_online,
        input.estimated_cost.map(Money::cents),
        input.active,
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?;
    row.try_into()
}

/// Approves the allergen data `reviewed`, which must be exactly what the product holds
/// now, so an edit made while someone was reviewing is never approved unseen.
///
/// # Errors
/// [`Error::NotFound`], or [`Error::AllergensChanged`] when `reviewed` is out of date.
pub async fn approve_allergens(
    conn: &mut PgConnection,
    id: ProductId,
    reviewed: &AllergenInfo,
    by: StaffUserId,
    clock: &dyn Clock,
) -> Result<Product, Error> {
    let row = sqlx::query_as!(
        Row,
        r#"UPDATE product SET
            allergens_approved_by = $2,
            allergens_approved_at = timestamptz 'epoch' + $3::bigint * interval '1 microsecond'
        WHERE id = $1 AND allergens @> $4 AND allergens <@ $4
            AND may_contain @> $5 AND may_contain <@ $5 AND ingredient_text = $6
        RETURNING id, name_nl, description_nl, photo_path, price_incl_cents, vat_rate,
            pay_what_you_want, min_price_cents, max_price_cents, unit_weight_g,
            dough_g_per_piece, freezer_days, lead_time_hours, allergens, may_contain,
            ingredient_text, allergens_approved_by,
            (extract(epoch FROM allergens_approved_at) * 1000000)::bigint AS allergens_approved_at_us,
            packaged, sold_online, estimated_cost_cents, active"#,
        id.get(),
        by.get(),
        db::time::to_micros(clock.now()),
        &allergens_to_db(&reviewed.allergens),
        &allergens_to_db(&reviewed.may_contain),
        reviewed.ingredient_text,
    )
    .fetch_optional(&mut *conn)
    .await?;
    match row {
        Some(row) => row.try_into(),
        None => {
            get(conn, id).await?;
            Err(Error::AllergensChanged)
        }
    }
}

/// # Errors
/// [`Error::NotFound`] when there is no such product.
pub async fn get(conn: &mut PgConnection, id: ProductId) -> Result<Product, Error> {
    sqlx::query_as!(
        Row,
        r#"SELECT id, name_nl, description_nl, photo_path, price_incl_cents, vat_rate,
            pay_what_you_want, min_price_cents, max_price_cents, unit_weight_g,
            dough_g_per_piece, freezer_days, lead_time_hours, allergens, may_contain,
            ingredient_text, allergens_approved_by,
            (extract(epoch FROM allergens_approved_at) * 1000000)::bigint AS allergens_approved_at_us,
            packaged, sold_online, estimated_cost_cents, active
        FROM product WHERE id = $1"#,
        id.get(),
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?
    .try_into()
}

/// Active products, by name.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn list_active(conn: &mut PgConnection) -> Result<Vec<Product>, Error> {
    sqlx::query_as!(
        Row,
        r#"SELECT id, name_nl, description_nl, photo_path, price_incl_cents, vat_rate,
            pay_what_you_want, min_price_cents, max_price_cents, unit_weight_g,
            dough_g_per_piece, freezer_days, lead_time_hours, allergens, may_contain,
            ingredient_text, allergens_approved_by,
            (extract(epoch FROM allergens_approved_at) * 1000000)::bigint AS allergens_approved_at_us,
            packaged, sold_online, estimated_cost_cents, active
        FROM product WHERE active ORDER BY name_nl, id"#,
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(Product::try_from)
    .collect()
}

/// Every product, inactive ones included, by name. For the back office.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn list_all(conn: &mut PgConnection) -> Result<Vec<Product>, Error> {
    sqlx::query_as!(
        Row,
        r#"SELECT id, name_nl, description_nl, photo_path, price_incl_cents, vat_rate,
            pay_what_you_want, min_price_cents, max_price_cents, unit_weight_g,
            dough_g_per_piece, freezer_days, lead_time_hours, allergens, may_contain,
            ingredient_text, allergens_approved_by,
            (extract(epoch FROM allergens_approved_at) * 1000000)::bigint AS allergens_approved_at_us,
            packaged, sold_online, estimated_cost_cents, active
        FROM product ORDER BY name_nl, id"#,
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(Product::try_from)
    .collect()
}

/// Active products with `sold_online` on, by name.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn list_sold_online(conn: &mut PgConnection) -> Result<Vec<Product>, Error> {
    sqlx::query_as!(
        Row,
        r#"SELECT id, name_nl, description_nl, photo_path, price_incl_cents, vat_rate,
            pay_what_you_want, min_price_cents, max_price_cents, unit_weight_g,
            dough_g_per_piece, freezer_days, lead_time_hours, allergens, may_contain,
            ingredient_text, allergens_approved_by,
            (extract(epoch FROM allergens_approved_at) * 1000000)::bigint AS allergens_approved_at_us,
            packaged, sold_online, estimated_cost_cents, active
        FROM product WHERE active AND sold_online ORDER BY name_nl, id"#,
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(Product::try_from)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_allergen_codes_are_corrupt() {
        assert!(matches!(
            allergens_from_db(&["gluten".to_owned()]),
            Err(Error::Corrupt(_))
        ));
    }
}
