//! Whether a batch's product may get a label (F17, F18, F18a, ADR 0046).
//!
//! The allergens and ingredient list are computed from the batch's own lines. They may go
//! on a label when they equal the product's approved computed values, or when someone
//! approved exactly these values for the batch. Anything else blocks the label, also a
//! change in the order by weight alone.

use std::collections::BTreeSet;

use catalog::{Approval, ComputedAllergens, Difference, IngredientList, Product, company};
use domain::{Allergen, BatchId, Clock, ProductId, StaffUserId};
use orders::audit::{self, Actor, Entry};
use serde_json::json;
use sqlx::{Acquire, PgConnection};

use crate::Error;
use crate::batches;

/// Why a batch's product may not get a label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelBlock {
    /// The kitchen-wide may-contain list is not approved.
    KitchenNotApproved,
    /// The product's own allergen data is not approved.
    ProductNotApproved,
    /// The batch, or a sub-recipe in it, has no lines to weigh.
    Unweighable,
    /// The batch's computed values differ from the product's approved computed values
    /// (`None` when the product's were entered by hand), and nobody approved them for
    /// this batch.
    Differs {
        computed: ComputedAllergens,
        approved: Option<ComputedAllergens>,
        difference: Difference,
    },
}

/// Whose approval covers the label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelSource {
    Product,
    Batch,
}

/// What a label for the batch carries (F18, F18a).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelContent {
    pub allergens: BTreeSet<Allergen>,
    /// The kitchen-wide list without what the product contains.
    pub may_contain: BTreeSet<Allergen>,
    pub ingredients: IngredientList,
    pub source: LabelSource,
    /// The batch has the product's ingredients in another order by weight: warn before
    /// printing (F18).
    pub order_changed: bool,
}

/// Everything the batch's label check looks at, for the back office.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelCheck {
    pub product: Product,
    /// `None` when the batch cannot be weighed.
    pub computed: Option<ComputedAllergens>,
    /// The product's approved computed values; `None` when not approved or entered by hand.
    pub product_values: Option<ComputedAllergens>,
    pub batch_values: Option<ComputedAllergens>,
    pub batch_approval: Option<Approval>,
    pub kitchen_approved: bool,
    pub kitchen: BTreeSet<Allergen>,
}

impl LabelCheck {
    /// # Errors
    /// The [`LabelBlock`] when the label may not be printed.
    pub fn outcome(&self) -> Result<LabelContent, LabelBlock> {
        if !self.kitchen_approved {
            return Err(LabelBlock::KitchenNotApproved);
        }
        if self.product.approval.is_none() {
            return Err(LabelBlock::ProductNotApproved);
        }
        let computed = self.computed.as_ref().ok_or(LabelBlock::Unweighable)?;
        let source = if self.product_values.as_ref() == Some(computed) {
            LabelSource::Product
        } else if self.batch_approval.is_some() && self.batch_values.as_ref() == Some(computed) {
            LabelSource::Batch
        } else {
            let difference = self.product_values.as_ref().map_or(
                Difference {
                    allergens: computed.allergens != self.product.allergen_info.allergens,
                    ingredients: true,
                    order: false,
                },
                |approved| Difference::between(computed, approved),
            );
            return Err(LabelBlock::Differs {
                computed: computed.clone(),
                approved: self.product_values.clone(),
                difference,
            });
        };
        Ok(LabelContent {
            allergens: computed.allergens.clone(),
            may_contain: self
                .kitchen
                .difference(&computed.allergens)
                .copied()
                .collect(),
            ingredients: computed.ingredients.clone(),
            source,
            order_changed: self.product_values.as_ref().is_some_and(|approved| {
                computed
                    .ingredients
                    .same_ingredients_other_order(&approved.ingredients)
            }),
        })
    }
}

struct BatchValues {
    allergens: Option<Vec<String>>,
    ingredient_names: Option<Vec<String>>,
    ingredient_emphasis: Option<Vec<bool>>,
    allergens_approved_by: Option<i64>,
    approved_at_us: Option<i64>,
}

fn allergens_from_db(codes: &[String]) -> Result<BTreeSet<Allergen>, Error> {
    codes
        .iter()
        .map(|code| code.parse().map_err(|e| Error::Corrupt(format!("{e}"))))
        .collect()
}

fn list_from_db(names: Vec<String>, emphasis: Vec<bool>) -> Result<IngredientList, Error> {
    if names.len() != emphasis.len() {
        return Err(Error::Corrupt("batch ingredient list".to_owned()));
    }
    Ok(IngredientList(
        names
            .into_iter()
            .zip(emphasis)
            .map(|(name, emphasised)| catalog::ListedIngredient { name, emphasised })
            .collect(),
    ))
}

/// The batch's computed values; `None` when it cannot be weighed.
async fn computed(
    conn: &mut PgConnection,
    batch: &batches::Batch,
) -> Result<Option<ComputedAllergens>, Error> {
    let formula = batches::formula(conn, batch).await?;
    match recipes::rollup::compute_stored(conn, &formula).await {
        Ok(computed) => Ok(Some(computed)),
        Err(recipes::Error::Unweighable) => Ok(None),
        Err(other) => Err(other.into()),
    }
}

/// # Errors
/// [`Error::NotFound`] when the batch does not exist or does not make the product.
pub async fn check(
    conn: &mut PgConnection,
    batch: BatchId,
    product: ProductId,
) -> Result<LabelCheck, Error> {
    let loaded = batches::get(conn, batch).await?;
    if !loaded.outputs.iter().any(|o| o.product_id == product) {
        return Err(Error::NotFound);
    }
    let computed = computed(conn, &loaded).await?;
    let product = catalog::products::get(conn, product).await?;
    let kitchen = company::kitchen_may_contain(conn).await?;
    let row = sqlx::query_as!(
        BatchValues,
        r#"SELECT allergens, ingredient_names, ingredient_emphasis, allergens_approved_by,
            (extract(epoch FROM allergens_approved_at) * 1000000)::bigint AS approved_at_us
        FROM batch WHERE id = $1"#,
        batch.get(),
    )
    .fetch_one(&mut *conn)
    .await?;
    let (batch_values, batch_approval) = match row {
        BatchValues {
            allergens: Some(allergens),
            ingredient_names: Some(names),
            ingredient_emphasis: Some(emphasis),
            allergens_approved_by: Some(by),
            approved_at_us: Some(at),
        } => (
            Some(ComputedAllergens {
                allergens: allergens_from_db(&allergens)?,
                ingredients: list_from_db(names, emphasis)?,
            }),
            Some(Approval {
                by: StaffUserId::new(by),
                at: db::time::from_micros(at)?,
            }),
        ),
        _ => (None, None),
    };
    let product_values = product
        .approval
        .and(product.ingredient_list.as_ref())
        .map(|list| ComputedAllergens {
            allergens: product.allergen_info.allergens.clone(),
            ingredients: list.clone(),
        });
    Ok(LabelCheck {
        product,
        computed,
        product_values,
        batch_values,
        batch_approval,
        kitchen_approved: kitchen.approval.is_some(),
        kitchen: kitchen.allergens,
    })
}

/// What a label for `product` from `batch` carries, or why it may not be printed. The
/// seam for product labels (F18) and freezer labels (F18a).
///
/// # Errors
/// [`Error::LabelBlocked`] with the reason; [`Error::NotFound`] when the batch does not
/// exist or does not make the product.
pub async fn label_allowed(
    conn: &mut PgConnection,
    batch: BatchId,
    product: ProductId,
) -> Result<LabelContent, Error> {
    check(conn, batch, product)
        .await?
        .outcome()
        .map_err(Error::LabelBlocked)
}

/// Approves the batch's computed values for its labels, when they equal `reviewed`. Valid
/// for as long as the batch computes the same values.
///
/// # Errors
/// [`Error::AllergensChanged`] when the batch now computes other values or cannot be
/// weighed; [`Error::NotFound`] for an unknown batch.
pub async fn approve_batch(
    conn: &mut PgConnection,
    batch: BatchId,
    reviewed: &ComputedAllergens,
    by: StaffUserId,
    clock: &dyn Clock,
) -> Result<(), Error> {
    let mut tx = conn.begin().await?;
    sqlx::query_scalar!("SELECT id FROM batch WHERE id = $1 FOR UPDATE", batch.get())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(Error::NotFound)?;
    let loaded = batches::get(&mut tx, batch).await?;
    if computed(&mut tx, &loaded).await?.as_ref() != Some(reviewed) {
        return Err(Error::AllergensChanged);
    }
    let allergens: Vec<String> = reviewed
        .allergens
        .iter()
        .map(|a| a.code().to_owned())
        .collect();
    let (names, emphasis): (Vec<String>, Vec<bool>) = reviewed
        .ingredients
        .0
        .iter()
        .map(|i| (i.name.clone(), i.emphasised))
        .unzip();
    sqlx::query!(
        "UPDATE batch SET allergens = $2, ingredient_names = $3, ingredient_emphasis = $4,
            allergens_approved_by = $5,
            allergens_approved_at = timestamptz 'epoch' + $6::bigint * interval '1 microsecond'
        WHERE id = $1",
        batch.get(),
        &allergens,
        &names,
        &emphasis,
        by.get(),
        db::time::to_micros(clock.now()),
    )
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        &Entry {
            entity: "batch",
            entity_id: batch.get(),
            action: "label_approved",
            before: None,
            after: Some(
                json!({ "allergens": allergens, "ingredients": names, "emphasis": emphasis }),
            ),
            actor: Actor::Staff(by),
            at: clock.now(),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
