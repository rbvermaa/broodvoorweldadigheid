//! Ingredient prices with a valid-from date, and the standard cost of products on a date
//! (F16, ADR 0045). Prices are only added, never changed, so history stays.

use std::collections::HashMap;

use db::time::{from_epoch_days, from_micros, to_epoch_days, to_micros};
use domain::{Clock, IngredientId, Money, ProductId, StaffUserId};
use jiff::Timestamp;
use jiff::civil::Date;
use sqlx::PgConnection;

use crate::Error;
use crate::cost::{PieceCost, Prices, UnitPrice};
use crate::formula::ScaleError;
use crate::needs::{ProductRecipe, product_recipes};

/// The highest price paid, in cents: €100.000.
pub const MAX_PRICE_CENTS: i64 = 10_000_000;
/// The heaviest package, in grams: 1000 kg.
pub const MAX_WEIGHT_G: u32 = 1_000_000;
/// The highest price per kilogram, rounded, in cents: €100.000.
pub const MAX_PER_KG_CENTS: i64 = 10_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPrice {
    pub ingredient_id: IngredientId,
    pub price: UnitPrice,
    /// A date in Europe/Amsterdam; the price counts from the start of that day.
    pub valid_from: Date,
    pub by: Option<StaffUserId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Price {
    pub id: i64,
    pub ingredient_id: IngredientId,
    /// As entered. A price from before prices per package is per 1000 g.
    pub price: UnitPrice,
    pub valid_from: Date,
    pub created_at: Timestamp,
}

struct Row {
    id: i64,
    ingredient_id: i64,
    price_per_kg_cents: i64,
    price_cents: Option<i64>,
    weight_g: Option<i32>,
    valid_day: i32,
    created_us: i64,
}

impl TryFrom<Row> for Price {
    type Error = Error;

    fn try_from(row: Row) -> Result<Self, Error> {
        let price = match (row.price_cents, row.weight_g) {
            (Some(cents), Some(weight)) => u32::try_from(weight)
                .ok()
                .and_then(|weight| UnitPrice::new(Money::from_cents(cents), weight))
                .ok_or_else(|| Error::Corrupt("ingredient_price.weight_g".to_owned()))?,
            _ => UnitPrice::per_kg(Money::from_cents(row.price_per_kg_cents)),
        };
        Ok(Self {
            id: row.id,
            ingredient_id: IngredientId::new(row.ingredient_id),
            price,
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

/// Why a price cannot be stored; `None` when it can.
#[must_use]
pub fn problem(price: UnitPrice) -> Option<&'static str> {
    if !(0..=MAX_PRICE_CENTS).contains(&price.price().cents()) {
        Some("price")
    } else if price.weight_g() > MAX_WEIGHT_G {
        Some("weight")
    } else if price.per_kg_cents().cents() > MAX_PER_KG_CENTS {
        Some("per_kg")
    } else {
        None
    }
}

/// Adds a price from `valid_from` on. A second price with the same date replaces the first
/// from then on; both stay in the history.
///
/// # Errors
/// [`Error::Invalid`] with the [`problem`] of the price; [`Error::NotFound`] for an
/// unknown ingredient.
pub async fn add(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    price: &NewPrice,
) -> Result<Price, Error> {
    if let Some(field) = problem(price.price) {
        return Err(Error::Invalid(field));
    }
    sqlx::query_as!(
        Row,
        r#"INSERT INTO ingredient_price
            (ingredient_id, price_per_kg_cents, price_cents, weight_g, valid_from, created_at,
                created_by)
        SELECT id, $2, $3, $4, date 'epoch' + $5::integer,
            timestamptz 'epoch' + $6::bigint * interval '1 microsecond', $7
        FROM ingredient WHERE id = $1
        RETURNING id, ingredient_id, price_per_kg_cents, price_cents, weight_g,
            (valid_from - date 'epoch') AS "valid_day!",
            (extract(epoch FROM created_at) * 1000000)::bigint AS "created_us!""#,
        price.ingredient_id.get(),
        price.price.per_kg_cents().cents(),
        price.price.price().cents(),
        i32::try_from(price.price.weight_g()).map_err(|_| Error::Invalid("weight"))?,
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
        r#"SELECT id, ingredient_id, price_per_kg_cents, price_cents, weight_g,
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
            price_cents, weight_g,
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

/// [`valid_on`] as the prices costing works with.
///
/// # Errors
/// As [`valid_on`].
pub async fn prices_on(conn: &mut PgConnection, date: Date) -> Result<Prices, Error> {
    Ok(valid_on(conn, date)
        .await?
        .into_iter()
        .map(|(id, price)| (id, price.price))
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
