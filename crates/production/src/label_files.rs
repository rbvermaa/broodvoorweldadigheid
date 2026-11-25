//! Product labels (F18) and freezer labels (F18a) of a batch as files for the Niimbot
//! app, and the pieces of a batch that went into the freezer (ADR 0047).
//!
//! Every label starts with the F17 label check (`label_check::label_allowed`); a
//! [`BatchLabel`] can only come from [`label`], and the files only from a [`BatchLabel`].

use catalog::label_settings;
use domain::{BatchId, Clock, ProductId};
use i18n::Translator;
use jiff::civil::Date;
use orders::audit::{self, Actor, Entry};
use serde_json::json;
use sqlx::{Acquire, PgConnection};

use crate::batches::{self, MAX_PIECES};
use crate::{Error, label_check};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelKind {
    Product,
    Freezer,
}

impl LabelKind {
    pub const ALL: [Self; 2] = [Self::Product, Self::Freezer];

    /// In URLs and file names; never change.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Product => "product",
            Self::Freezer => "vriezer",
        }
    }

    #[must_use]
    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.slug() == slug)
    }
}

/// What a label needs beyond the allergen check, and is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelGap {
    /// The company has no address.
    NoAddress,
    /// A product label for a product not sold packaged.
    NotPackaged,
    NoShelfLife,
    NoStorage,
    NoFreezerPeriod,
    /// No actual pieces recorded yet.
    NotBaked,
    /// Baked, but no good pieces.
    NoPieces,
    NoFrozenPieces,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Checked;

/// A label that passed every check, with how many to print. Read-only, so nothing can
/// change it after the check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchLabel {
    kind: LabelKind,
    label: labels::Label,
    copies: u32,
    order_changed: bool,
    checked: Checked,
}

impl BatchLabel {
    #[must_use]
    pub const fn kind(&self) -> LabelKind {
        self.kind
    }

    #[must_use]
    pub const fn label(&self) -> &labels::Label {
        &self.label
    }

    #[must_use]
    pub const fn copies(&self) -> u32 {
        self.copies
    }

    /// The batch has the ingredients in another order by weight: warn before printing.
    #[must_use]
    pub const fn order_changed(&self) -> bool {
        self.order_changed
    }
}

/// Pieces of a batch's product in the freezer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frozen {
    pub pieces: u32,
    /// The bakery's date when they were first recorded.
    pub on: Date,
}

struct OutputRow {
    actual_pieces: Option<i32>,
    waste_pieces: Option<i32>,
    frozen_pieces: Option<i32>,
    frozen_on: Option<i32>,
}

struct Output {
    good: Option<u32>,
    frozen: Option<Frozen>,
}

fn label_ingredient(listed: catalog::ListedIngredient) -> labels::Ingredient {
    labels::Ingredient {
        name: listed.name,
        bold: listed.emphasised,
        parts: listed.parts.into_iter().map(label_ingredient).collect(),
    }
}

fn count(value: i32) -> Result<u32, Error> {
    u32::try_from(value).map_err(|_| Error::Corrupt(format!("{value} pieces")))
}

async fn output(
    conn: &mut PgConnection,
    batch: BatchId,
    product: ProductId,
    lock: bool,
) -> Result<Output, Error> {
    let row = if lock {
        sqlx::query_as!(
            OutputRow,
            r#"SELECT actual_pieces, waste_pieces, frozen_pieces,
                (frozen_on - date '1970-01-01') AS "frozen_on: i32"
            FROM batch_output WHERE batch_id = $1 AND product_id = $2 FOR UPDATE"#,
            batch.get(),
            product.get(),
        )
        .fetch_optional(conn)
        .await?
    } else {
        sqlx::query_as!(
            OutputRow,
            r#"SELECT actual_pieces, waste_pieces, frozen_pieces,
                (frozen_on - date '1970-01-01') AS "frozen_on: i32"
            FROM batch_output WHERE batch_id = $1 AND product_id = $2"#,
            batch.get(),
            product.get(),
        )
        .fetch_optional(conn)
        .await?
    }
    .ok_or(Error::NotFound)?;
    let good = match (row.actual_pieces, row.waste_pieces) {
        (Some(actual), Some(waste)) => Some(count(actual - waste)?),
        _ => None,
    };
    let frozen = match (row.frozen_pieces, row.frozen_on) {
        (Some(pieces), Some(on)) => Some(Frozen {
            pieces: count(pieces)?,
            on: db::time::from_epoch_days(on)?,
        }),
        (None, None) => None,
        _ => return Err(Error::Corrupt("frozen pieces without a date".to_owned())),
    };
    Ok(Output { good, frozen })
}

/// The pieces of `product` from `batch` in the freezer, if any.
///
/// # Errors
/// [`Error::NotFound`] when the batch does not make the product.
pub async fn frozen(
    conn: &mut PgConnection,
    batch: BatchId,
    product: ProductId,
) -> Result<Option<Frozen>, Error> {
    Ok(output(conn, batch, product, false).await?.frozen)
}

/// Records how many good pieces went into the freezer; 0 clears it. The freeze date is
/// the bakery's date of the first recording, so a correction keeps it.
///
/// # Errors
/// [`Error::NotBaked`] before actual pieces are recorded; [`Error::Invalid`] for more
/// pieces than the good ones; [`Error::NotFound`] when the batch does not make the
/// product.
pub async fn record_frozen(
    conn: &mut PgConnection,
    batch: BatchId,
    product: ProductId,
    pieces: u32,
    actor: Actor,
    clock: &dyn Clock,
) -> Result<(), Error> {
    let mut tx = conn.begin().await?;
    batches::lock(&mut tx, batch).await?;
    let before = output(&mut tx, batch, product, true).await?;
    let good = before.good.ok_or(Error::NotBaked)?;
    if pieces > good.min(MAX_PIECES) {
        return Err(Error::Invalid("frozen pieces"));
    }
    let now = clock.now();
    let frozen = (pieces > 0).then(|| Frozen {
        pieces,
        on: before
            .frozen
            .map_or_else(|| clock.now_in_bakery().date(), |f| f.on),
    });
    sqlx::query!(
        "UPDATE batch_output SET frozen_pieces = $3,
            frozen_on = date '1970-01-01' + $4::integer
        WHERE batch_id = $1 AND product_id = $2",
        batch.get(),
        product.get(),
        frozen
            .map(|f| i32::try_from(f.pieces))
            .transpose()
            .map_err(|_| Error::Invalid("frozen pieces"))?,
        frozen.map(|f| db::time::to_epoch_days(f.on)),
    )
    .execute(&mut *tx)
    .await?;
    let entry = |f: Option<Frozen>| {
        f.map(
            |f| json!({ "product_id": product.get(), "pieces": f.pieces, "on": f.on.to_string() }),
        )
    };
    audit::record(
        &mut tx,
        &Entry {
            entity: "batch",
            entity_id: batch.get(),
            action: "frozen",
            before: entry(before.frozen),
            after: entry(frozen),
            actor,
            at: now,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// The label of `kind` for `product` from `batch`, after the F17 label check.
///
/// # Errors
/// [`Error::LabelBlocked`] when the allergen data may not go on a label, checked first;
/// then [`Error::LabelGap`] for what else is missing; [`Error::NotFound`] when the batch
/// does not make the product.
pub async fn label(
    conn: &mut PgConnection,
    batch: BatchId,
    product: ProductId,
    kind: LabelKind,
) -> Result<BatchLabel, Error> {
    let content = label_check::label_allowed(conn, batch, product).await?;
    let loaded = batches::get(conn, batch).await?;
    let output = output(conn, batch, product, false).await?;
    let settings = label_settings::get(conn, product).await?;
    let stored = catalog::products::get(conn, product).await?;
    let contact = catalog::company::contact(conn).await?;
    let address = contact
        .address
        .ok_or(Error::LabelGap(LabelGap::NoAddress))?;
    let (kind_data, best_before, copies) = match kind {
        LabelKind::Product => {
            if !settings.packaged {
                return Err(Error::LabelGap(LabelGap::NotPackaged));
            }
            let days = settings
                .shelf_life_days
                .ok_or(Error::LabelGap(LabelGap::NoShelfLife))?;
            if settings.storage.is_empty() {
                return Err(Error::LabelGap(LabelGap::NoStorage));
            }
            let good = output
                .good
                .ok_or(Error::LabelGap(LabelGap::NotBaked))?
                .saturating_sub(output.frozen.map_or(0, |f| f.pieces));
            if good == 0 {
                return Err(Error::LabelGap(LabelGap::NoPieces));
            }
            let baked_on = bakedays::days::get(conn, loaded.bake_day_id).await?.date;
            (
                labels::Kind::Product {
                    storage: settings.storage,
                },
                labels::best_before(baked_on, days)?,
                good,
            )
        }
        LabelKind::Freezer => {
            let days = settings
                .freezer_days
                .ok_or(Error::LabelGap(LabelGap::NoFreezerPeriod))?;
            let frozen = output
                .frozen
                .ok_or(Error::LabelGap(LabelGap::NoFrozenPieces))?;
            (
                labels::Kind::Freezer {
                    frozen_on: frozen.on,
                },
                labels::best_before(frozen.on, days)?,
                frozen.pieces,
            )
        }
    };
    Ok(BatchLabel {
        kind,
        label: labels::Label {
            kind: kind_data,
            product: stored.name_nl,
            ingredients: content
                .ingredients
                .0
                .into_iter()
                .map(label_ingredient)
                .collect(),
            may_contain: content.may_contain,
            net_weight_g: u32::try_from(stored.unit_weight_g)
                .map_err(|_| Error::Corrupt("unit weight".to_owned()))?,
            best_before,
            batch_code: loaded.code.to_string(),
            business: labels::Business {
                name: contact.name,
                address,
            },
        },
        copies,
        order_changed: content.order_changed,
        checked: Checked,
    })
}

/// A label PNG and its file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelFile {
    pub name: String,
    pub png: labels::Png,
}

fn base_name(label: &BatchLabel) -> String {
    format!("{}-{}", label.label.batch_code, label.kind.slug())
}

/// The PNGs: one, or a front and a back.
///
/// # Errors
/// [`Error::Label`] when the label cannot be drawn or does not fit.
pub fn files(label: &BatchLabel, t: &Translator) -> Result<Vec<LabelFile>, Error> {
    let pngs = labels::render(&label.label, t)?;
    let base = base_name(label);
    Ok(pngs
        .into_iter()
        .enumerate()
        .map(|(i, png)| LabelFile {
            name: format!("{base}-{}.png", i + 1),
            png,
        })
        .collect())
}

/// The CSV row of the label text for the Niimbot app's import; `files` are its PNGs, so
/// there is no CSV for a label that cannot be drawn.
///
/// # Errors
/// [`Error::Label`] when the CSV cannot be made.
pub fn csv(label: &BatchLabel, files: &[LabelFile], t: &Translator) -> Result<String, Error> {
    let file = files
        .iter()
        .map(|f| f.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Ok(labels::csv(
        &[labels::CsvRow {
            label: &label.label,
            copies: label.copies,
            file: &file,
        }],
        t,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip_through_their_slug() {
        for kind in LabelKind::ALL {
            assert_eq!(LabelKind::from_slug(kind.slug()), Some(kind));
        }
        assert_eq!(LabelKind::from_slug("anders"), None);
    }
}
