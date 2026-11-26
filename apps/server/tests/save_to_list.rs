#![forbid(unsafe_code)]
//! Every back-office POST route and where it goes after a save (ADR 0052). A new form
//! fails this test until it is added here, with the list it returns to or why it stays.

use std::collections::BTreeSet;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum After {
    /// An object's own create or edit form: back to this list, row named and highlighted.
    List(&'static str),
    /// A section of a hub page (Bread Drop, batch, order, recipe, version, product).
    Hub,
    /// An inline htmx edit in a table, or a live check or preview of one field.
    Inline,
    /// A settings page: it stays and says "Opgeslagen".
    Settings,
    /// A search or a one-off action with its own result page.
    Action,
}

use After::{Action, Hub, Inline, List, Settings};

/// (file, route, handler, after). Sorted by file, then as the router lists them.
const ROUTES: &[(&str, &str, &str, After)] = &[
    (
        "bake_days.rs",
        "/staff/bake-days/new",
        "create",
        List("/staff/bake-days"),
    ),
    (
        "bake_days.rs",
        "/staff/bake-days/{id}/schedule",
        "schedule",
        Hub,
    ),
    (
        "bake_days.rs",
        "/staff/bake-days/{id}/charity",
        "charity",
        Hub,
    ),
    (
        "bake_days.rs",
        "/staff/bake-days/{id}/range",
        "set_product",
        Hub,
    ),
    (
        "bake_days.rs",
        "/staff/bake-days/{id}/range/{product}",
        "update_product",
        Inline,
    ),
    (
        "bake_days.rs",
        "/staff/bake-days/{id}/range/{product}/remove",
        "remove_product",
        Inline,
    ),
    ("bake_days.rs", "/staff/bake-days/{id}/copy", "copy", Hub),
    (
        "bake_days.rs",
        "/staff/bake-days/{id}/repeat",
        "repeat",
        Hub,
    ),
    (
        "bake_days.rs",
        "/staff/bake-days/{id}/slots",
        "add_slot",
        Hub,
    ),
    (
        "bake_days.rs",
        "/staff/bake-days/{id}/slots/{slot}",
        "update_slot",
        Hub,
    ),
    (
        "bake_days.rs",
        "/staff/bake-days/{id}/slots/{slot}/remove",
        "remove_slot",
        Hub,
    ),
    ("bake_days.rs", "/staff/bake-days/{id}/open", "open", Hub),
    ("bake_days.rs", "/staff/bake-days/{id}/close", "close", Hub),
    (
        "batch_labels.rs",
        "/staff/bake-days/{id}/batches/{batch}/etiket/{product}/approve",
        "approve",
        Hub,
    ),
    (
        "batches.rs",
        "/staff/bake-days/{id}/batches/new",
        "start",
        Hub,
    ),
    (
        "batches.rs",
        "/staff/bake-days/{id}/batches/{batch}",
        "save",
        Hub,
    ),
    (
        "batches.rs",
        "/staff/bake-days/{id}/batches/{batch}/preview",
        "preview",
        Inline,
    ),
    (
        "batches.rs",
        "/staff/bake-days/{id}/batches/{batch}/lines",
        "add_line",
        Hub,
    ),
    (
        "batches.rs",
        "/staff/bake-days/{id}/batches/{batch}/mixed",
        "mixed",
        Hub,
    ),
    (
        "batches.rs",
        "/staff/bake-days/{id}/batches/{batch}/actuals",
        "actuals",
        Hub,
    ),
    (
        "batches.rs",
        "/staff/bake-days/{id}/batches/{batch}/promote",
        "promote",
        Hub,
    ),
    (
        "business.rs",
        "/staff/business-accounts/new",
        "create",
        List("/staff/business-accounts"),
    ),
    (
        "business.rs",
        "/staff/business-accounts/{id}",
        "update",
        List("/staff/business-accounts"),
    ),
    (
        "business.rs",
        "/staff/business-accounts/{id}/invoices",
        "mark_invoiced",
        Action,
    ),
    (
        "business.rs",
        "/staff/business-accounts/{id}/order",
        "place_order",
        Action,
    ),
    (
        "charities.rs",
        "/staff/charities/new",
        "create",
        List("/staff/charities"),
    ),
    ("charities.rs", "/staff/charities/check", "check", Inline),
    (
        "charities.rs",
        "/staff/charities/{id}",
        "update",
        List("/staff/charities"),
    ),
    (
        "charities.rs",
        "/staff/charities/{id}/archive",
        "archive",
        List("/staff/charities"),
    ),
    (
        "costing.rs",
        "/staff/costing/ingredients/{id}",
        "add_price",
        Hub,
    ),
    (
        "costing.rs",
        "/staff/costing/ingredients/{id}/preview",
        "preview_price",
        Inline,
    ),
    ("customers.rs", "PATH", "search", Action),
    (
        "customers.rs",
        "/staff/customers/{id}/erase",
        "erase",
        Action,
    ),
    ("extras.rs", "/staff/bake-days/{id}/extras", "add", Inline),
    (
        "extras.rs",
        "/staff/bake-days/{id}/extras/{extra}",
        "update",
        Inline,
    ),
    (
        "extras.rs",
        "/staff/bake-days/{id}/extras/{extra}/remove",
        "remove",
        Inline,
    ),
    (
        "ingredients.rs",
        "/staff/ingredients/new",
        "create",
        List("/staff/ingredients"),
    ),
    (
        "ingredients.rs",
        "/staff/ingredients/{id}",
        "update",
        List("/staff/ingredients"),
    ),
    ("kitchen.rs", "PATH", "save", Settings),
    ("kitchen.rs", "APPROVE_PATH", "approve", Settings),
    (
        "label_files.rs",
        "/staff/bake-days/{id}/batches/{batch}/etiketten/{product}/vriezer",
        "freeze",
        Hub,
    ),
    (
        "lists.rs",
        "/staff/bake-days/{id}/pickup-list/{order}/picked-up",
        "picked_up",
        Inline,
    ),
    (
        "lists.rs",
        "/staff/bake-days/{id}/pickup-list/{order}/undo",
        "undo",
        Inline,
    ),
    (
        "locations.rs",
        "/staff/locations/new",
        "create",
        List("/staff/locations"),
    ),
    (
        "locations.rs",
        "/staff/locations/{id}",
        "update",
        List("/staff/locations"),
    ),
    (
        "locations.rs",
        "/staff/locations/{id}/windows/new",
        "create_window",
        List("/staff/locations/{id}"),
    ),
    (
        "locations.rs",
        "/staff/locations/windows/check",
        "check_window",
        Inline,
    ),
    (
        "locations.rs",
        "/staff/locations/{id}/windows/{window}",
        "update_window",
        List("/staff/locations/{id}"),
    ),
    ("orders.rs", "/staff/orders/{id}/pickup", "move_pickup", Hub),
    (
        "orders.rs",
        "/staff/orders/{id}/picked-up",
        "picked_up",
        Hub,
    ),
    ("orders.rs", "/staff/orders/{id}/cancel", "cancel", Hub),
    (
        "orders.rs",
        "/staff/orders/{id}/refunds/{refund}/retry",
        "retry_refund",
        Hub,
    ),
    (
        "payouts.rs",
        "/staff/reports/payouts/{id}/transfers",
        "record",
        Hub,
    ),
    (
        "polls.rs",
        "/staff/polls/new",
        "create",
        List("/staff/polls"),
    ),
    (
        "polls.rs",
        "/staff/polls/{id}",
        "update",
        List("/staff/polls"),
    ),
    (
        "product_labels.rs",
        "/staff/products/{id}/etiket",
        "save",
        List("/staff/products"),
    ),
    (
        "products.rs",
        "/staff/products/new",
        "create",
        List("/staff/products"),
    ),
    ("products.rs", "/staff/products/check", "check", Inline),
    (
        "products.rs",
        "/staff/products/{id}",
        "update",
        List("/staff/products"),
    ),
    (
        "products.rs",
        "/staff/products/{id}/approve",
        "approve",
        Hub,
    ),
    (
        "products.rs",
        "/staff/products/{id}/approve-computed",
        "approve_computed",
        Hub,
    ),
    (
        "recipes.rs",
        "/staff/recipes/new",
        "create",
        List("/staff/recipes"),
    ),
    (
        "recipes.rs",
        "/staff/recipes/{id}",
        "update",
        List("/staff/recipes"),
    ),
    (
        "recipes.rs",
        "/staff/recipes/{id}/versions",
        "new_version",
        Hub,
    ),
    ("recipes.rs", "/staff/recipes/{id}/etiket", "set_label", Hub),
    (
        "recipes.rs",
        "/staff/recipes/{id}/products",
        "link_product",
        Hub,
    ),
    (
        "recipes.rs",
        "/staff/recipes/{id}/products/{product}/remove",
        "unlink_product",
        Hub,
    ),
    (
        "recipes.rs",
        "/staff/recipes/{id}/versions/{version}/edit",
        "editor::save_details",
        Hub,
    ),
    (
        "recipes.rs",
        "/staff/recipes/{id}/versions/{version}/lines",
        "editor::add_line",
        Inline,
    ),
    (
        "recipes.rs",
        "/staff/recipes/{id}/versions/{version}/lines/{position}",
        "editor::set_line",
        Inline,
    ),
    (
        "recipes.rs",
        "/staff/recipes/{id}/versions/{version}/lines/{position}/remove",
        "editor::remove_line",
        Inline,
    ),
    (
        "recipes.rs",
        "/staff/recipes/{id}/versions/{version}/method",
        "method::set_kind",
        Inline,
    ),
    (
        "recipes.rs",
        "/staff/recipes/{id}/versions/{version}/text",
        "method::save_text",
        Inline,
    ),
    (
        "recipes.rs",
        "/staff/recipes/{id}/versions/{version}/steps",
        "method::add_step",
        Inline,
    ),
    (
        "recipes.rs",
        "/staff/recipes/{id}/versions/{version}/steps/{position}",
        "method::set_step",
        Inline,
    ),
    (
        "recipes.rs",
        "/staff/recipes/{id}/versions/{version}/steps/{position}/remove",
        "method::remove_step",
        Inline,
    ),
    (
        "recipes.rs",
        "/staff/recipes/{id}/versions/{version}/steps/{position}/up",
        "method::step_up",
        Inline,
    ),
    (
        "recipes.rs",
        "/staff/recipes/{id}/versions/{version}/steps/{position}/down",
        "method::step_down",
        Inline,
    ),
    (
        "recipes.rs",
        "/staff/recipes/{id}/versions/{version}/freeze",
        "freeze",
        Hub,
    ),
    (
        "recipes.rs",
        "/staff/recipes/{id}/versions/{version}/current",
        "make_current",
        Hub,
    ),
    (
        "reports.rs",
        "/staff/reports/contribution/costs",
        "add_cost",
        Hub,
    ),
    ("settings.rs", "PATH", "save", Settings),
    ("settings.rs", "ADDRESS_PATH", "save_address", Settings),
    (
        "sick_day.rs",
        "/staff/bake-days/{id}/cancel",
        "cancel",
        Action,
    ),
];

/// (file, route, handler) of every `.route(...)` with a `post(handler)` in the back office.
/// A route is its string literal, or the name of the const it uses.
fn post_routes() -> BTreeSet<(String, String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/backoffice");
    let mut files = Vec::new();
    let mut todo = vec![dir.clone()];
    while let Some(next) = todo.pop() {
        for entry in std::fs::read_dir(&next).expect("readable directory") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                todo.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
    }
    let mut found = BTreeSet::new();
    for path in files {
        let file = path
            .strip_prefix(&dir)
            .expect("under backoffice")
            .to_str()
            .expect("utf-8")
            .to_owned();
        let source = std::fs::read_to_string(&path).expect("readable source");
        for (at, _) in source.match_indices(".route(") {
            let rest = &source[at + ".route(".len()..];
            let mut depth = 1;
            let end = rest
                .char_indices()
                .find(|&(_, c)| {
                    depth += match c {
                        '(' => 1,
                        ')' => -1,
                        _ => 0,
                    };
                    depth == 0
                })
                .map(|(i, _)| i)
                .expect("closing parenthesis");
            let call = &rest[..end];
            let Some(handler) = call.split("post(").nth(1) else {
                continue;
            };
            let handler = handler.split(')').next().expect("handler").trim();
            let first = call.trim_start();
            let route = match first.strip_prefix('"') {
                Some(literal) => literal.split('"').next().expect("literal"),
                None => first.split(',').next().expect("const").trim(),
            };
            found.insert((file.clone(), route.to_owned(), handler.to_owned()));
        }
    }
    found
}

#[test]
fn every_post_route_says_where_it_goes_after_a_save() {
    let listed: BTreeSet<(String, String, String)> = ROUTES
        .iter()
        .map(|(file, route, handler, _)| {
            (
                (*file).to_owned(),
                (*route).to_owned(),
                (*handler).to_owned(),
            )
        })
        .collect();
    assert_eq!(listed.len(), ROUTES.len(), "a route is listed twice");
    let found = post_routes();
    let missing: Vec<_> = found.difference(&listed).collect();
    let gone: Vec<_> = listed.difference(&found).collect();
    assert!(
        missing.is_empty() && gone.is_empty(),
        "add new POST routes to ROUTES with where they go (ADR 0052): {missing:#?}\n\
         remove routes that no longer exist: {gone:#?}"
    );
}

#[test]
fn every_object_form_goes_back_to_a_list_it_belongs_under() {
    for (file, route, handler, after) in ROUTES {
        if let List(list) = after {
            assert!(
                route.starts_with(list),
                "{file} {handler}: {route} should go back to a list above it, not {list}"
            );
        }
    }
}
