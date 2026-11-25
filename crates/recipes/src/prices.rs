//! Ingredient prices with a valid-from date, and the standard cost of products on a date
//! (F16, ADR 0045). Prices are only added, never changed, so history stays.

use std::collections::HashMap;

use db::time::{from_epoch_days, from_micros, to_epoch_days, to_micros};
use domain::{Clock, IngredientId, Money, ProductId, StaffUserId};
use jiff::Timestamp;
use jiff::civil::Date;
use sqlx::PgConnection;

use crate::Error;
use crate::cost::{PieceCost, Prices};
use crate::formula::ScaleError;
use crate::needs::{ProductRecipe, product_recipes};

/// The highest price per kilogram, in cents: €100.000.
pub const MAX_PER_KG_CENTS: i64 = 10_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPrice {
    pub ingredient_id: IngredientId,
    pub per_kg: Money,
    /// A date in Europe/Amsterdam; the price counts from the start of that day.
    pub valid_from: Date,
    pub by: Option<StaffUserId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Price {
    pub id: i64,
    pub ingredient_id: IngredientId,
    pub per_kg: Money,
    pub valid_from: Date,
    pub created_at: Timestamp,
}

struct Row {
    id: i64,
    ingredient_id: i64,
    price_per_kg_cents: i64,
    valid_day: i32,
    created_us: i64,
}

impl TryFrom<Row> for Price {
    type Error = Error;

    fn try_from(row: Row) -> Result<Self, Error> {
        Ok(Self {
            id: row.id,
            ingredient_id: IngredientId::new(row.ingredient_id),
            per_kg: Money::from_cents(row.price_per_kg_cents),
            valid_from: from_epoch_days(row.valid_day)?,
            created_at: from_micros(row.created_us)?,
        })
    }
}

/// Today in the bakery's time zone: the date whose prices are the current ones.
#[must_use]
pub fn today(clock: &dyn Clock) -> Date {
    clock.now_in_bakery().date()
}

/// Adds a price from `valid_from` on. A second price with the same date replaces the first
/// from then on; both stay in the history.
///
/// # Errors
/// [`Error::Invalid`] with `price` below zero or above [`MAX_PER_KG_CENTS`];
/// [`Error::NotFound`] for an unknown ingredient.
pub async fn add(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    price: &NewPrice,
) -> Result<Price, Error> {
    if !(0..=MAX_PER_KG_CENTS).contains(&price.per_kg.cents()) {
        return Err(Error::Invalid("price"));
    }
    sqlx::query_as!(
        Row,
        r#"INSERT INTO ingredient_price
            (ingredient_id, price_per_kg_cents, valid_from, created_at, created_by)
        SELECT id, $2, date 'epoch' + $3::integer,
            timestamptz 'epoch' + $4::bigint * interval '1 microsecond', $5
        FROM ingredient WHERE id = $1
        RETURNING id, ingredient_id, price_per_kg_cents,
            (valid_from - date 'epoch') AS "valid_day!",
            (extract(epoch FROM created_at) * 1000000)::bigint AS "created_us!""#,
        price.ingredient_id.get(),
        price.per_kg.cents(),
        to_epoch_days(price.valid_from),
        to_micros(clock.now()),
        price.by.map(StaffUserId::get),
    )
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?
    .try_into()
}

/// Every price of an ingredient, the one that counts last first: newest valid-from date,
/// then the one entered last.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn history(
    conn: &mut PgConnection,
    ingredient: IngredientId,
) -> Result<Vec<Price>, Error> {
    sqlx::query_as!(
        Row,
        r#"SELECT id, ingredient_id, price_per_kg_cents,
            (valid_from - date 'epoch') AS "valid_day!",
            (extract(epoch FROM created_at) * 1000000)::bigint AS "created_us!"
        FROM ingredient_price WHERE ingredient_id = $1
        ORDER BY valid_from DESC, id DESC"#,
        ingredient.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(Price::try_from)
    .collect()
}

/// The price of every ingredient that has one on `date`: the latest valid-from date on or
/// before it, and of several on that date the one entered last.
///
/// # Errors
/// [`Error::Db`] or [`Error::Corrupt`].
pub async fn valid_on(
    conn: &mut PgConnection,
    date: Date,
) -> Result<HashMap<IngredientId, Price>, Error> {
    sqlx::query_as!(
        Row,
        r#"SELECT DISTINCT ON (ingredient_id) id, ingredient_id, price_per_kg_cents,
            (valid_from - date 'epoch') AS "valid_day!",
            (extract(epoch FROM created_at) * 1000000)::bigint AS "created_us!"
        FROM ingredient_price WHERE valid_from <= date 'epoch' + $1::integer
        ORDER BY ingredient_id, valid_from DESC, id DESC"#,
        to_epoch_days(date),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| {
        let price = Price::try_from(row)?;
        Ok((price.ingredient_id, price))
    })
    .collect()
}

/// [`valid_on`] as prices per kilogram.
///
/// # Errors
/// As [`valid_on`].
pub async fn prices_on(conn: &mut PgConnection, date: Date) -> Result<Prices, Error> {
    Ok(valid_on(conn, date)
        .await?
        .into_iter()
        .map(|(id, price)| (id, price.per_kg))
        .collect())
}

/// Why a product has no standard cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoCost {
    NoRecipe,
    NoVersion,
    /// The version in use, or one of its sub-recipes, has no lines.
    NoLines,
    NoDough,
}

/// A product's standard cost: its recipe's version in use at the prices of one date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandardCost {
    pub product: ProductRecipe,
    pub cost: Result<PieceCost, NoCost>,
}

impl StandardCost {
    /// One piece; `None` without a cost or while an ingredient has no price.
    #[must_use]
    pub fn per_piece(&self) -> Option<Money> {
        self.cost.as_ref().ok().and_then(PieceCost::per_piece)
    }
}

/// The standard cost of each product in `products` that exists, at the prices valid on
/// `date`, by product id.
///
/// # Errors
/// [`Error::Db`], [`Error::Corrupt`] or [`Error::Nesting`].
pub async fn standard_costs(
    conn: &mut PgConnection,
    products: &[ProductId],
    date: Date,
) -> Result<Vec<StandardCost>, Error> {
    let prices = prices_on(conn, date).await?;
    let mut costs = Vec::new();
    for product in product_recipes(conn, products).await? {
        let cost = match &product.recipe {
            None => Err(NoCost::NoRecipe),
            Some(recipe) => match recipe.version {
                None => Err(NoCost::NoVersion),
                Some(_) if product.dough_g_per_piece == 0 => Err(NoCost::NoDough),
                Some((version, _)) => {
                    let formula = crate::recipes::formula(conn, version).await?;
                    PieceCost::new(&formula, product.dough_g_per_piece, &prices)
                        .map_err(|ScaleError::Empty(_)| NoCost::NoLines)
                }
            },
        };
        costs.push(StandardCost { product, cost });
    }
    Ok(costs)
}
