//! The data each Typst template reads: every text already translated and formatted, so
//! the templates hold layout only.

use bakedays::Purpose;
use i18n::{Amsterdam, FluentArgs, Translator, decimal};
use orders::{OrderStatus, PaymentMode};
use serde_json::{Value, json};

use crate::{Error, IngredientNeeds, PickupList, PickupOrder, ProductionList, weight};

fn count(n: u64) -> String {
    decimal(i64::try_from(n).unwrap_or(i64::MAX), 0)
}

fn purpose_label(t: &Translator, purpose: Purpose) -> Result<String, Error> {
    let id = match purpose {
        Purpose::Freezer => "production-extra-freezer",
        Purpose::SumUp => "production-extra-sumup",
        Purpose::Samples => "production-extra-samples",
        Purpose::Other => "production-extra-other",
    };
    Ok(t.t(id, None)?)
}

pub(crate) fn production(list: &ProductionList, t: &Translator) -> Result<Value, Error> {
    let mut columns = vec![t.t("production-ordered", None)?];
    for purpose in Purpose::ALL {
        columns.push(purpose_label(t, purpose)?);
    }
    columns.push(t.t("production-to-bake", None)?);

    let row = |ordered: u64, extras: &crate::Extras, to_bake: u64| {
        let mut cells = vec![count(ordered)];
        cells.extend(Purpose::ALL.map(|p| count(extras.get(p))));
        cells.push(count(to_bake));
        cells
    };
    let mut sum = crate::Extras::default();
    let mut ordered = 0;
    let products: Vec<Value> = list
        .products
        .iter()
        .map(|p| {
            ordered += p.ordered;
            for purpose in Purpose::ALL {
                sum.add(purpose, p.extras.get(purpose));
            }
            json!({ "name": p.name, "cells": row(p.ordered, &p.extras, p.to_bake()) })
        })
        .collect();
    Ok(json!({
        "title": t.t("production-title", None)?,
        "date": t.long_date(list.day.date),
        "labels": {
            "product": t.t("production-product", None)?,
            "columns": columns,
            "sum": t.t("production-sum", None)?,
            "empty": t.t("production-empty", None)?,
        },
        "products": products,
        "sum": row(ordered, &sum, ordered + sum.total()),
    }))
}

fn status_label(t: &Translator, order: &PickupOrder) -> Result<String, Error> {
    let id = match (order.status, order.payment_mode) {
        (OrderStatus::PickedUp, _) => "pickup-list-status-picked-up",
        (OrderStatus::Confirmed, PaymentMode::OnAccount) => "pickup-list-status-on-account",
        (OrderStatus::Confirmed, PaymentMode::Online) => "pickup-list-status-confirmed",
        (OrderStatus::Paid, _) => "pickup-list-status-paid",
        (other, _) => return Err(Error::Corrupt(format!("{other} order on the pickup list"))),
    };
    Ok(t.t(id, None)?)
}

pub(crate) fn pickup(list: &PickupList, t: &Translator) -> Result<Value, Error> {
    let zone = Amsterdam::new();
    let mut slots = Vec::with_capacity(list.slots.len());
    for slot in &list.slots {
        let mut args = FluentArgs::new();
        args.set(
            "range",
            zone.time_range(slot.slot.starts_at, slot.slot.ends_at)?,
        );
        args.set("day", t.short_date(zone.date(slot.slot.starts_at)));
        args.set("location", slot.location.as_str());
        let mut orders = Vec::with_capacity(slot.orders.len());
        for order in &slot.orders {
            let mut lines = Vec::with_capacity(order.lines.len());
            for line in &order.lines {
                let mut args = FluentArgs::new();
                args.set("qty", count(u64::from(line.qty)));
                args.set("product", line.name.as_str());
                lines.push(t.t("pickup-list-line", Some(&args))?);
            }
            orders.push(json!({
                "number": order.number.to_string(),
                "name": order.customer_name,
                "phone": order.customer_phone.as_deref().unwrap_or(""),
                "status": status_label(t, order)?,
                "picked_up": order.picked_up,
                "lines": lines,
            }));
        }
        slots.push(json!({
            "heading": t.t("pickup-list-slot", Some(&args))?,
            "orders": orders,
        }));
    }
    Ok(json!({
        "title": t.t("pickup-list-title", None)?,
        "date": t.long_date(list.day.date),
        "labels": {
            "number": t.t("pickup-list-number", None)?,
            "customer": t.t("pickup-list-customer", None)?,
            "items": t.t("pickup-list-items", None)?,
            "status": t.t("pickup-list-status", None)?,
            "done": t.t("pickup-list-done", None)?,
            "empty": t.t("pickup-list-empty", None)?,
        },
        "slots": slots,
    }))
}

pub(crate) fn needs(needs: &IngredientNeeds, t: &Translator) -> Result<Value, Error> {
    let mut ingredients = Vec::with_capacity(needs.ingredients.len());
    for need in &needs.ingredients {
        ingredients.push(json!({
            "name": need.ingredient.name,
            "amount": weight(t, need.grams)?,
        }));
    }
    let mut doughs = Vec::with_capacity(needs.doughs.len());
    for dough in &needs.doughs {
        let mut args = FluentArgs::new();
        args.set("recipe", dough.recipe.as_str());
        args.set("version", dough.version.to_string());
        let mut products = Vec::with_capacity(dough.products.len());
        for product in &dough.products {
            let mut args = FluentArgs::new();
            args.set("pieces", count(product.pieces));
            args.set("product", product.name.as_str());
            products.push(t.t("needs-dough-product", Some(&args))?);
        }
        doughs.push(json!({
            "name": t.t("needs-dough-name", Some(&args))?,
            "products": products.join(", "),
            "amount": weight(t, dough.grams.total())?,
        }));
    }
    let mut missing = Vec::with_capacity(needs.missing.len());
    for product in &needs.missing {
        missing.push(json!({
            "name": product.name,
            "pieces": count(product.pieces),
            "reason": t.t(product.reason.message_id(), None)?,
        }));
    }
    let sum = needs.ingredients.iter().map(|need| need.grams).sum();
    Ok(json!({
        "title": t.t("needs-title", None)?,
        "date": t.long_date(needs.day.date),
        "basis": t.t("needs-basis-recipes", None)?,
        "labels": {
            "ingredient": t.t("needs-ingredient", None)?,
            "amount": t.t("needs-amount", None)?,
            "sum": t.t("needs-sum", None)?,
            "empty": t.t("needs-empty", None)?,
            "doughs": t.t("needs-doughs", None)?,
            "recipe": t.t("needs-recipe", None)?,
            "products": t.t("needs-products", None)?,
            "missing": t.t("needs-missing", None)?,
            "missing_intro": t.t("needs-missing-intro", None)?,
            "product": t.t("needs-product", None)?,
            "pieces": t.t("needs-pieces", None)?,
            "reason": t.t("needs-reason", None)?,
        },
        "ingredients": ingredients,
        "sum": weight(t, sum)?,
        "doughs": doughs,
        "missing": missing,
    }))
}
