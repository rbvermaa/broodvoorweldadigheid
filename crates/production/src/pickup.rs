use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use bakedays::{BakeDay, PickupSlot, days, pickup};
use domain::{BakeDayId, OrderId, ProductId};
use i18n::Translator;
use orders::{OrderNumber, OrderStatus, PaymentMode};
use sqlx::PgConnection;

use crate::{Error, paper, pdf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickupLine {
    pub product_id: ProductId,
    pub name: String,
    pub qty: u32,
}

/// An order to hand out, with the name and phone entered for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickupOrder {
    pub order_id: OrderId,
    pub number: OrderNumber,
    pub customer_name: String,
    pub customer_phone: Option<String>,
    pub status: OrderStatus,
    pub payment_mode: PaymentMode,
    pub picked_up: bool,
    pub lines: Vec<PickupLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotPickups {
    pub slot: PickupSlot,
    pub location: String,
    /// By customer name, then order number.
    pub orders: Vec<PickupOrder>,
}

/// Orders per pickup slot for one bake day (F8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickupList {
    pub day: BakeDay,
    /// Every slot of the day by start time, also slots without orders.
    pub slots: Vec<SlotPickups>,
}

/// The pickup list of a bake day: paid, confirmed and picked-up orders. Call it outside a
/// transaction; all reads see one snapshot of the database.
///
/// # Errors
/// [`Error::BakeDays`] with `NotFound` for an unknown day, or a database error.
pub async fn pickup_list(conn: &mut PgConnection, day: BakeDayId) -> Result<PickupList, Error> {
    let mut tx = crate::snapshot(conn).await?;
    let day = days::get(&mut tx, day).await?;
    let mut locations: BTreeMap<_, String> = BTreeMap::new();
    let mut slots = Vec::new();
    for slot in days::list_slots(&mut tx, day.id).await? {
        let location = match locations.entry(slot.location_id) {
            Entry::Occupied(known) => known.get().clone(),
            Entry::Vacant(new) => {
                let location = pickup::get_location(&mut tx, slot.location_id).await?;
                new.insert(location.name).clone()
            }
        };
        slots.push(SlotPickups {
            location,
            slot,
            orders: Vec::new(),
        });
    }

    let mut names: BTreeMap<ProductId, String> = BTreeMap::new();
    for order in orders::order::list_firm(&mut tx, day.id).await? {
        let mut lines = Vec::with_capacity(order.lines.len());
        for line in &order.lines {
            let name = match names.entry(line.product_id) {
                Entry::Occupied(known) => known.get().clone(),
                Entry::Vacant(new) => {
                    let product = catalog::products::get(&mut tx, line.product_id).await?;
                    new.insert(product.name_nl).clone()
                }
            };
            lines.push(PickupLine {
                product_id: line.product_id,
                name,
                qty: line.qty,
            });
        }
        let slot = slots
            .iter_mut()
            .find(|s| s.slot.id == order.pickup_slot_id)
            .ok_or_else(|| {
                Error::Corrupt(format!(
                    "order {} has a slot of another day",
                    order.id.get()
                ))
            })?;
        slot.orders.push(PickupOrder {
            order_id: order.id,
            number: order.number,
            customer_name: order.customer_name,
            customer_phone: order.customer_phone,
            status: order.status,
            payment_mode: order.payment_mode,
            picked_up: order.status == OrderStatus::PickedUp,
            lines,
        });
    }
    tx.commit().await?;
    for slot in &mut slots {
        slot.orders.sort_by(|a, b| {
            a.customer_name
                .to_lowercase()
                .cmp(&b.customer_name.to_lowercase())
                .then(a.number.cmp(&b.number))
        });
    }
    Ok(PickupList { day, slots })
}

/// The pickup list as an A4 PDF, with a box to tick per order.
///
/// # Errors
/// [`Error::I18n`] or [`Error::Pdf`].
pub fn pickup_pdf(list: &PickupList, t: &Translator) -> Result<Vec<u8>, Error> {
    let data = paper::pickup(list, t)?;
    pdf::render(&pdf::PICKUP, &data, format!("pickup-{}", list.day.date))
}
