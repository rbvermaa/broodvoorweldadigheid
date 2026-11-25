use std::collections::BTreeSet;

use domain::{Allergen, Clock, Money, PayWhatYouWant, ProductId, StaffUserId, VatRate};
use jiff::Timestamp;
use sqlx::{Acquire, PgConnection};

use crate::Error;
use crate::company::KitchenMayContain;
use crate::ingredient_list::{ComputedAllergens, IngredientList};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AllergenInfo {
    pub allergens: BTreeSet<Allergen>,
    pub ingredient_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Approval {
    pub by: StaffUserId,
    pub at: Timestamp,
}

/// A change to the product's recipe, or to an ingredient in it, took the allergen approval
/// away (database trigger, ADR 0037).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalCleared {
    Recipe,
    Ingredient,
}

impl std::str::FromStr for ApprovalCleared {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        match s {
            "recipe" => Ok(Self::Recipe),
            "ingredient" => Ok(Self::Ingredient),
            other => Err(Error::Corrupt(format!("cleared reason {other}"))),
        }
    }
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
    /// The computed list the allergen data came from (F17); `None` when the ingredient
    /// text was entered by hand.
    pub ingredient_list: Option<IngredientList>,
    /// Made from a recipe: only the values the recipe computes can be approved (F17).
    pub has_recipe: bool,
    pub approval: Option<Approval>,
    /// Why the approval was last taken away, until the next approval.
    pub approval_cleared: Option<ApprovalCleared>,
    /// Goes up whenever the recipe changes in a way that clears the approval; an approval
    /// names the revision it reviewed.
    pub allergen_revision: i64,
    pub packaged: bool,
    pub sold_online: bool,
    pub estimated_cost: Option<Money>,
    pub active: bool,
}

/// What a customer sees of a product's allergens (F10): what it contains, and the
/// kitchen-wide may-contain list without the allergens it already contains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedAllergens {
    pub contains: BTreeSet<Allergen>,
    pub may_contain: BTreeSet<Allergen>,
    pub ingredient_text: String,
}

impl PublishedAllergens {
    #[must_use]
    pub fn new(info: &AllergenInfo, kitchen: &BTreeSet<Allergen>) -> Self {
        Self {
            contains: info.allergens.clone(),
            may_contain: kitchen.difference(&info.allergens).copied().collect(),
            ingredient_text: info.ingredient_text.clone(),
        }
    }
}

impl Product {
    /// Both the product's allergen data and the kitchen-wide may-contain list are
    /// approved. Only then may the product be shown with allergens or ordered.
    #[must_use]
    pub const fn allergens_approved(&self, kitchen: &KitchenMayContain) -> bool {
        self.approval.is_some() && kitchen.approval.is_some()
    }

    #[must_use]
    pub fn published_allergens(&self, kitchen: &KitchenMayContain) -> Option<PublishedAllergens> {
        self.allergens_approved(kitchen)
            .then(|| PublishedAllergens::new(&self.allergen_info, &kitchen.allergens))
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
    ingredient_text: String,
    allergens_approved_by: Option<i64>,
    allergens_approved_at_us: Option<i64>,
    allergens_cleared_reason: Option<String>,
    allergens_recipe_rev: i64,
    ingredient_names: Option<Vec<String>>,
    ingredient_emphasis: Option<Vec<bool>>,
    has_recipe: bool,
    packaged: bool,
    sold_online: bool,
    estimated_cost_cents: Option<i64>,
    active: bool,
}

pub(crate) fn allergens_from_db(codes: &[String]) -> Result<BTreeSet<Allergen>, Error> {
    codes
        .iter()
        .map(|code| code.parse().map_err(|e| Error::Corrupt(format!("{e}"))))
        .collect()
}

pub(crate) fn allergens_to_db(allergens: &BTreeSet<Allergen>) -> Vec<String> {
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
                ingredient_text: row.ingredient_text,
            },
            ingredient_list: IngredientList::from_db(
                row.ingredient_names,
                row.ingredient_emphasis,
            )?,
            has_recipe: row.has_recipe,
            approval,
            approval_cleared: row
                .allergens_cleared_reason
                .as_deref()
                .map(str::parse)
                .transpose()?,
            allergen_revision: row.allergens_recipe_rev,
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
            dough_g_per_piece, freezer_days, lead_time_hours, allergens,
            ingredient_text, packaged, sold_online, estimated_cost_cents, active
        ) VALUES ($1, $2, $3, $4, $5::bigint, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)
        RETURNING id, name_nl, description_nl, photo_path, price_incl_cents, vat_rate,
            pay_what_you_want, min_price_cents, max_price_cents, unit_weight_g,
            dough_g_per_piece, freezer_days, lead_time_hours, allergens,
            ingredient_text, allergens_approved_by,
            (extract(epoch FROM allergens_approved_at) * 1000000)::bigint AS allergens_approved_at_us,
            allergens_cleared_reason, allergens_recipe_rev, ingredient_names, ingredient_emphasis,
            recipe_id IS NOT NULL AS "has_recipe!",
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

/// Replaces every editable field. A change to the allergens or ingredient text
/// clears the allergen approval (database trigger) and the computed ingredient list.
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
            lead_time_hours = $13, allergens = $14, ingredient_text = $15,
            packaged = $16, sold_online = $17, estimated_cost_cents = $18, active = $19,
            ingredient_names = CASE WHEN allergens @> $14 AND allergens <@ $14
                AND ingredient_text = $15 THEN ingredient_names END,
            ingredient_emphasis = CASE WHEN allergens @> $14 AND allergens <@ $14
                AND ingredient_text = $15 THEN ingredient_emphasis END
        WHERE id = $1
        RETURNING id, name_nl, description_nl, photo_path, price_incl_cents, vat_rate,
            pay_what_you_want, min_price_cents, max_price_cents, unit_weight_g,
            dough_g_per_piece, freezer_days, lead_time_hours, allergens,
            ingredient_text, allergens_approved_by,
            (extract(epoch FROM allergens_approved_at) * 1000000)::bigint AS allergens_approved_at_us,
            allergens_cleared_reason, allergens_recipe_rev, ingredient_names, ingredient_emphasis,
            recipe_id IS NOT NULL AS "has_recipe!",
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

/// Approves hand-entered allergen data `reviewed` at recipe revision `revision`
/// ([`Product::allergen_revision`]); both must be what the product holds now, so an edit
/// or recipe change made while someone was reviewing is never approved unseen. A product
/// made from a recipe takes only computed values ([`approve_computed`], F17).
///
/// # Errors
/// [`Error::NotFound`], [`Error::AllergensChanged`] when `reviewed` is out of date, or
/// [`Error::ComputedOnly`] for a product with a recipe.
pub async fn approve_allergens(
    conn: &mut PgConnection,
    id: ProductId,
    reviewed: &AllergenInfo,
    revision: i64,
    by: StaffUserId,
    clock: &dyn Clock,
) -> Result<Product, Error> {
    approve_stored(conn, id, reviewed, revision, by, clock, true).await
}

async fn approve_stored(
    conn: &mut PgConnection,
    id: ProductId,
    reviewed: &AllergenInfo,
    revision: i64,
    by: StaffUserId,
    clock: &dyn Clock,
    hand: bool,
) -> Result<Product, Error> {
    let row = sqlx::query_as!(
        Row,
        r#"UPDATE product SET
            allergens_approved_by = $2,
            allergens_approved_at = timestamptz 'epoch' + $3::bigint * interval '1 microsecond'
        WHERE id = $1 AND allergens @> $4 AND allergens <@ $4
            AND ingredient_text = $5 AND allergens_recipe_rev = $6
            AND NOT ($7 AND recipe_id IS NOT NULL)
        RETURNING id, name_nl, description_nl, photo_path, price_incl_cents, vat_rate,
            pay_what_you_want, min_price_cents, max_price_cents, unit_weight_g,
            dough_g_per_piece, freezer_days, lead_time_hours, allergens,
            ingredient_text, allergens_approved_by,
            (extract(epoch FROM allergens_approved_at) * 1000000)::bigint AS allergens_approved_at_us,
            allergens_cleared_reason, allergens_recipe_rev, ingredient_names, ingredient_emphasis,
            recipe_id IS NOT NULL AS "has_recipe!",
            packaged, sold_online, estimated_cost_cents, active"#,
        id.get(),
        by.get(),
        db::time::to_micros(clock.now()),
        &allergens_to_db(&reviewed.allergens),
        reviewed.ingredient_text,
        revision,
        hand,
    )
    .fetch_optional(&mut *conn)
    .await?;
    match row {
        Some(row) => row.try_into(),
        None if hand && get(conn, id).await?.has_recipe => Err(Error::ComputedOnly),
        None => {
            get(conn, id).await?;
            Err(Error::AllergensChanged)
        }
    }
}

/// Stores `computed` as the product's allergens, ingredient text and ingredient list, and
/// approves them, when the product is still at recipe revision `revision` (F17). The
/// caller computes the values from the recipe in the same transaction, under the recipe
/// lock (`recipes::rollup::approve_product`).
///
/// # Errors
/// [`Error::NotFound`], or [`Error::AllergensChanged`] when the revision is out of date or
/// the product has no recipe.
pub async fn approve_computed(
    conn: &mut PgConnection,
    id: ProductId,
    computed: &ComputedAllergens,
    revision: i64,
    by: StaffUserId,
    clock: &dyn Clock,
) -> Result<Product, Error> {
    let mut tx = conn.begin().await?;
    let (names, emphasis) = computed.ingredients.to_db();
    let stored = sqlx::query!(
        "UPDATE product SET allergens = $2, ingredient_text = $3, ingredient_names = $4,
            ingredient_emphasis = $5
        WHERE id = $1 AND allergens_recipe_rev = $6 AND recipe_id IS NOT NULL",
        id.get(),
        &allergens_to_db(&computed.allergens),
        computed.ingredients.text(),
        &names,
        &emphasis,
        revision,
    )
    .execute(&mut *tx)
    .await?;
    if stored.rows_affected() == 0 {
        get(&mut tx, id).await?;
        return Err(Error::AllergensChanged);
    }
    let reviewed = AllergenInfo {
        allergens: computed.allergens.clone(),
        ingredient_text: computed.ingredients.text(),
    };
    let product = approve_stored(&mut tx, id, &reviewed, revision, by, clock, false).await?;
    tx.commit().await?;
    Ok(product)
}

/// Locks the rows of `ids` `FOR SHARE`, in id order, until the caller's transaction ends,
/// so their allergen approval cannot be cleared between a checkout's check and its commit
/// (ADR 0008, ADR 0038). Unknown ids are skipped.
///
/// # Errors
/// [`Error::Db`].
pub async fn lock_for_order(conn: &mut PgConnection, ids: &[ProductId]) -> Result<(), Error> {
    let ids: Vec<i64> = ids.iter().map(|id| id.get()).collect();
    sqlx::query_scalar!(
        "SELECT id FROM product WHERE id = ANY($1) ORDER BY id FOR SHARE",
        &ids,
    )
    .fetch_all(conn)
    .await?;
    Ok(())
}

/// # Errors
/// [`Error::NotFound`] when there is no such product.
pub async fn get(conn: &mut PgConnection, id: ProductId) -> Result<Product, Error> {
    sqlx::query_as!(
        Row,
        r#"SELECT id, name_nl, description_nl, photo_path, price_incl_cents, vat_rate,
            pay_what_you_want, min_price_cents, max_price_cents, unit_weight_g,
            dough_g_per_piece, freezer_days, lead_time_hours, allergens,
            ingredient_text, allergens_approved_by,
            (extract(epoch FROM allergens_approved_at) * 1000000)::bigint AS allergens_approved_at_us,
            allergens_cleared_reason, allergens_recipe_rev, ingredient_names, ingredient_emphasis,
            recipe_id IS NOT NULL AS "has_recipe!",
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
            dough_g_per_piece, freezer_days, lead_time_hours, allergens,
            ingredient_text, allergens_approved_by,
            (extract(epoch FROM allergens_approved_at) * 1000000)::bigint AS allergens_approved_at_us,
            allergens_cleared_reason, allergens_recipe_rev, ingredient_names, ingredient_emphasis,
            recipe_id IS NOT NULL AS "has_recipe!",
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
            dough_g_per_piece, freezer_days, lead_time_hours, allergens,
            ingredient_text, allergens_approved_by,
            (extract(epoch FROM allergens_approved_at) * 1000000)::bigint AS allergens_approved_at_us,
            allergens_cleared_reason, allergens_recipe_rev, ingredient_names, ingredient_emphasis,
            recipe_id IS NOT NULL AS "has_recipe!",
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
            dough_g_per_piece, freezer_days, lead_time_hours, allergens,
            ingredient_text, allergens_approved_by,
            (extract(epoch FROM allergens_approved_at) * 1000000)::bigint AS allergens_approved_at_us,
            allergens_cleared_reason, allergens_recipe_rev, ingredient_names, ingredient_emphasis,
            recipe_id IS NOT NULL AS "has_recipe!",
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
    fn may_contain_is_the_kitchen_list_minus_what_the_product_contains() {
        let info = AllergenInfo {
            allergens: BTreeSet::from([Allergen::CerealsContainingGluten, Allergen::Milk]),
            ingredient_text: "TARWEbloem, MELK".to_owned(),
        };
        let kitchen = BTreeSet::from([Allergen::Milk, Allergen::Nuts, Allergen::SesameSeeds]);
        let shown = PublishedAllergens::new(&info, &kitchen);
        assert_eq!(shown.contains, info.allergens);
        assert_eq!(
            shown.may_contain,
            BTreeSet::from([Allergen::Nuts, Allergen::SesameSeeds])
        );
        assert_eq!(shown.ingredient_text, "TARWEbloem, MELK");
        assert_eq!(
            PublishedAllergens::new(&info, &BTreeSet::new()).may_contain,
            BTreeSet::new()
        );
    }

    #[test]
    fn unknown_allergen_codes_are_corrupt() {
        assert!(matches!(
            allergens_from_db(&["gluten".to_owned()]),
            Err(Error::Corrupt(_))
        ));
    }
}
