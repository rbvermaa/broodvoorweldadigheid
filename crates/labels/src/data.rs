//! The data the Typst template reads: every text already translated and formatted, so
//! the template holds layout only.

use i18n::{FluentArgs, Translator, decimal, numeric_date};
use serde_json::{Value, json};

use crate::{Error, Kind, Label, Side, WIDTH_MM};

pub(crate) fn allergen_names(label: &Label, t: &Translator) -> Result<Vec<String>, Error> {
    label
        .may_contain
        .iter()
        .map(|a| Ok(t.t(&format!("allergen-{}", a.code()), None)?.to_lowercase()))
        .collect()
}

pub(crate) fn may_contain(label: &Label, t: &Translator) -> Result<Option<String>, Error> {
    if label.may_contain.is_empty() {
        return Ok(None);
    }
    let mut args = FluentArgs::new();
    args.set("allergens", allergen_names(label, t)?.join(", "));
    Ok(Some(t.t("labels-may-contain", Some(&args))?))
}

pub(crate) fn net_weight(label: &Label, t: &Translator) -> Result<String, Error> {
    let mut args = FluentArgs::new();
    args.set("grams", decimal(i64::from(label.net_weight_g), 0));
    Ok(t.t("labels-grams", Some(&args))?)
}

pub(crate) fn storage(label: &Label, t: &Translator) -> Result<String, Error> {
    Ok(match &label.kind {
        Kind::Product { storage } => storage.clone(),
        Kind::Freezer { .. } => t.t("labels-freezer-storage", None)?,
    })
}

pub(crate) fn business(label: &Label, t: &Translator) -> Result<String, Error> {
    let mut args = FluentArgs::new();
    args.set("name", label.business.name.as_str());
    args.set("address", label.business.address.as_str());
    Ok(t.t("labels-business", Some(&args))?)
}

/// The template's input for one side of `label`. Tests snapshot it.
///
/// # Errors
/// [`Error::I18n`] when a message is missing.
pub fn data(label: &Label, side: Side, t: &Translator) -> Result<Value, Error> {
    let field = |id: &str, value: String| -> Result<Value, Error> {
        Ok(json!({ "label": t.t(id, None)?, "value": value }))
    };
    let mut fields = vec![
        field("labels-net-weight", net_weight(label, t)?)?,
        field("labels-best-before", numeric_date(label.best_before))?,
    ];
    if let Kind::Freezer { frozen_on } = label.kind {
        fields.push(field("labels-frozen-on", numeric_date(frozen_on))?);
    }
    fields.push(field("labels-batch", label.batch_code.clone())?);
    let ingredients: Vec<Value> = label
        .ingredients
        .iter()
        .map(|i| json!({ "name": i.name, "bold": i.bold }))
        .collect();
    Ok(json!({
        "part": side.part(),
        "width_mm": WIDTH_MM,
        "product": label.product,
        "fields": fields,
        "storage": storage(label, t)?,
        "business": business(label, t)?,
        "ingredients_label": t.t("labels-ingredients", None)?,
        "ingredients": ingredients,
        "may_contain": may_contain(label, t)?,
    }))
}
