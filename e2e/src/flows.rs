use std::path::PathBuf;

use domain::OrderId;
use i18n::{FluentArgs, Translator};
use orders::OrderStatus;
use serde_json::Value;
use sqlx::PgPool;
use thirtyfour::prelude::*;

use crate::Error;
use crate::browser::{
    LAPTOP, PHONE, choose, click, fill, firefox, path, set_value, wait_for, wait_for_text,
};
use crate::mollie::{CHECKOUT_BASE, MockMollie, Payment, on_server};
use crate::seed::{self, OpenDrop};

const CUSTOMER_NAME: &str = "Test Klant";

/// The WCAG 2.2 AA rule sets of axe-core.
const AXE_TAGS: &str = r#"["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"]"#;

/// What every flow needs: the running server and WebDriver, the server's database and
/// the mock Mollie the server talks to.
pub struct Ctx {
    pub server: String,
    pub webdriver: String,
    pub pool: PgPool,
    pub mollie: MockMollie,
    pub axe_js: PathBuf,
    pub i18n: Translator,
}

impl Ctx {
    fn t(&self, id: &str) -> Result<String, Error> {
        Ok(self.i18n.t(id, None)?)
    }

    fn t_with(&self, id: &str, key: &str, value: String) -> Result<String, Error> {
        let mut args = FluentArgs::new();
        args.set(key.to_owned(), value);
        Ok(self.i18n.t(id, Some(&args))?)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.server.trim_end_matches('/'))
    }

    async fn order_status(&self, order: i64) -> Result<OrderStatus, Error> {
        let mut conn = self.pool.acquire().await?;
        Ok(orders::order::get(&mut conn, OrderId::new(order))
            .await?
            .status)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    BuyALoaf,
    StaffBakeDay,
    CustomerLogin,
    ChooseCharity,
    SickDayCancel,
    PickupList,
    GalleryAxe,
    RecipeEditor,
}

impl Flow {
    pub const ALL: [Self; 8] = [
        Self::BuyALoaf,
        Self::StaffBakeDay,
        Self::CustomerLogin,
        Self::ChooseCharity,
        Self::SickDayCancel,
        Self::PickupList,
        Self::GalleryAxe,
        Self::RecipeEditor,
    ];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::BuyALoaf => "buy a loaf, pay and get the confirmation (F4, F5, F9)",
            Self::StaffBakeDay => "staff plan a bake day and open it for ordering (F1)",
            Self::CustomerLogin => "a customer signs in with a magic link and out again",
            Self::ChooseCharity => "save a donation and choose its charity by link (F4d)",
            Self::SickDayCancel => "a sick-day cancel refunds and emails the customer (F2)",
            Self::PickupList => "tick an order off the pickup list on a phone, and undo (F8)",
            Self::GalleryAxe => "the gallery passes axe-core (WCAG 2.2 AA)",
            Self::RecipeEditor => {
                "edit a recipe line, add one by name and add a step on a phone (F14)"
            }
        }
    }

    fn window(self) -> (u32, u32) {
        match self {
            Self::PickupList | Self::RecipeEditor => PHONE,
            _ => LAPTOP,
        }
    }
}

/// Runs `flow` in a fresh browser session, which is closed whatever the outcome.
///
/// # Errors
/// What the flow or the WebDriver reports.
pub async fn run(ctx: &Ctx, flow: Flow) -> Result<(), Error> {
    let driver = firefox(&ctx.webdriver, flow.window()).await?;
    let result = match flow {
        Flow::BuyALoaf => buy_a_loaf(ctx, &driver).await,
        Flow::StaffBakeDay => staff_bake_day(ctx, &driver).await,
        Flow::CustomerLogin => customer_login(ctx, &driver).await,
        Flow::ChooseCharity => choose_charity(ctx, &driver).await,
        Flow::SickDayCancel => sick_day_cancel(ctx, &driver).await,
        Flow::PickupList => pickup_list(ctx, &driver).await,
        Flow::GalleryAxe => gallery_is_accessible(ctx, &driver).await,
        Flow::RecipeEditor => recipe_editor(ctx, &driver).await,
    };
    let quit = driver.quit().await;
    result?;
    quit?;
    Ok(())
}

/// A paid order, as the webhook left it.
struct Paid {
    order: i64,
    payment: Payment,
}

/// The tile shows the kitchen-wide may-contain list minus what the loaf contains (F10,
/// ADR 0038): the seed approves gluten and nuts and the loaf has gluten, so the only trace
/// is nuts.
async fn kitchen_traces_shown(ctx: &Ctx, driver: &WebDriver) -> Result<(), Error> {
    wait_for_text(driver, ".product-tile", &ctx.t("allergen-may-contain")?).await?;
    let traces = driver
        .find_all(By::Css(".product-tile .allergen-badge-may > span"))
        .await?;
    let mut shown = Vec::new();
    for badge in traces {
        shown.push(badge.text().await?);
    }
    if shown != [ctx.t("allergen-nuts")?] {
        return Err(format!("kitchen traces on the tile: {shown:?}").into());
    }
    Ok(())
}

/// Orders `qty` loaves on `drop` as `email`, with `donation` as the donation choice if
/// given, pays in the mock and sends the webhook. With `audit`, it opens the drop from the
/// Bread Drops list, and axe-core checks the home page, that list, the drop and checkout.
async fn order_and_pay(
    ctx: &Ctx,
    driver: &WebDriver,
    drop: &OpenDrop,
    email: &str,
    donation: Option<&str>,
    audit: bool,
) -> Result<Paid, Error> {
    if audit {
        driver.goto(ctx.url("/")).await?;
        axe(ctx, driver, "home").await?;
        driver.goto(ctx.url("/drops")).await?;
        axe(ctx, driver, "Bread Drops").await?;
        click(
            driver,
            &format!(".drop-rows a[href=\"/drops/{}\"]", drop.day),
        )
        .await?;
    } else {
        driver
            .goto(ctx.url(&format!("/drops/{}", drop.day)))
            .await?;
    }
    wait_for_text(driver, ".product-name", &drop.product_name).await?;
    kitchen_traces_shown(ctx, driver).await?;
    if audit {
        axe(ctx, driver, "drop").await?;
    }
    fill(driver, &format!("#aantal-{}", drop.product), "2").await?;
    click(driver, "form.product-order button[type=submit]").await?;
    wait_for_text(driver, "#mandje", &ctx.t("cart-added")?).await?;
    click(driver, "#mandje a.button-primary").await?;

    wait_for_text(driver, "h1", &ctx.t("checkout-title")?).await?;
    if audit {
        axe(ctx, driver, "checkout").await?;
    }
    fill(driver, "#naam", CUSTOMER_NAME).await?;
    fill(driver, "#email", email).await?;
    click(driver, "label[for=ophaalmoment-1]").await?;
    if let Some(value) = donation {
        choose(driver, "#donatie", value).await?;
    }
    // Firefox cannot load the checkout page, so the click reports an error page; the URL
    // check below decides whether the server sent the customer to Mollie.
    let submitted = driver
        .find(By::Css("form.form-stack button[type=submit]"))
        .await?
        .click()
        .await;
    let checkout = match wait_for("the Mollie checkout", || async {
        let url = driver.current_url().await?;
        Ok(checkout_url(url.as_str()))
    })
    .await
    {
        Ok(url) => url,
        Err(err) => return Err(submitted.err().map_or(err, Into::into)),
    };
    let id = checkout.trim_start_matches(CHECKOUT_BASE);
    let payment = ctx
        .mollie
        .payment(id)
        .ok_or_else(|| format!("the mock has no payment {id}"))?;
    if payment.value != "9.00" {
        return Err(format!("two loaves should cost 9.00, Mollie got {}", payment.value).into());
    }
    let order = payment
        .metadata
        .get("order_id")
        .and_then(Value::as_i64)
        .ok_or("the payment has no order_id")?;

    ctx.mollie.pay(id);
    let status = ctx.mollie.notify(id, &ctx.server).await?;
    if status != 200 {
        return Err(format!("the webhook answered {status}").into());
    }
    // Checked before any status page opens: that page asks Mollie itself and would settle
    // the payment even if the webhook did not.
    let after = ctx.order_status(order).await?;
    if after != OrderStatus::Paid {
        return Err(format!("after the webhook the order is {after:?}").into());
    }
    Ok(Paid { order, payment })
}

/// The confirmation email of `order`: recipient, subject and text.
async fn confirmation(ctx: &Ctx, order: i64) -> Result<(String, String, String), Error> {
    wait_for("the confirmation email", || async {
        Ok(sqlx::query_as(
            "SELECT to_address, subject, text_body FROM email_outbox
             WHERE kind = 'order_confirmation' AND dedupe_key = $1",
        )
        .bind(format!("order-confirmation:{order}"))
        .fetch_optional(&ctx.pool)
        .await?)
    })
    .await
}

/// The first link in `text` to `path_prefix` (such as `/inloggen/`), on the test server.
fn link_in(server: &str, text: &str, path_prefix: &str) -> Result<String, Error> {
    let link = text
        .split_whitespace()
        .find(|word| word.starts_with("https://") && word.contains(path_prefix))
        .ok_or_else(|| format!("no {path_prefix} link in:\n{text}"))?;
    let link = link.trim_end_matches(['.', ',', ')', '>', '"']);
    Ok(on_server(link, server)?.to_string())
}

async fn staff_sign_in(ctx: &Ctx, driver: &WebDriver, staff: i64) -> Result<(), Error> {
    driver.goto(ctx.url("/staff/login")).await?;
    wait_for_text(driver, "h1", &ctx.t("staff-dev-login-title")?).await?;
    fill(driver, "#staff_id", &staff.to_string()).await?;
    click(
        driver,
        "form[action=\"/staff/dev-login\"] button[type=submit]",
    )
    .await?;
    wait_for_text(driver, "h1", &ctx.t("staff-title")?).await
}

async fn buy_a_loaf(ctx: &Ctx, driver: &WebDriver) -> Result<(), Error> {
    let drop = seed::open_drop(&ctx.pool).await?;
    let email = seed::customer_email("koop");
    let Paid { order, payment } = order_and_pay(ctx, driver, &drop, &email, None, true).await?;
    let (to, subject, body) = confirmation(ctx, order).await?;
    let mut conn = ctx.pool.acquire().await?;
    let number = orders::order::get(&mut conn, OrderId::new(order))
        .await?
        .number;
    let checks = [
        (to == email, "is sent to the customer"),
        (
            subject
                == ctx.t_with(
                    "email-order-confirmation-subject",
                    "number",
                    number.to_string(),
                )?,
            "names the order number",
        ),
        (
            body.contains(&ctx.t_with("email-greeting", "name", CUSTOMER_NAME.to_owned())?),
            "greets the customer",
        ),
        (
            body.contains(&payment.redirect_url),
            "links to the status page",
        ),
    ];
    for (ok, what) in checks {
        if !ok {
            return Err(format!("the confirmation {what}: {subject}\n{body}").into());
        }
    }
    driver
        .goto(on_server(&payment.redirect_url, &ctx.server)?.as_str())
        .await?;
    wait_for_text(driver, "#bestelstatus", &ctx.t("order-status-paid")?).await?;
    axe(ctx, driver, "order status").await
}

async fn staff_bake_day(ctx: &Ctx, driver: &WebDriver) -> Result<(), Error> {
    let basics = seed::basics(&ctx.pool).await?;
    staff_sign_in(ctx, driver, basics.owner).await?;

    driver.goto(ctx.url("/staff/bake-days/new")).await?;
    wait_for_text(driver, "h1", &ctx.t("day-new")?).await?;
    set_value(driver, "#date", &seed::drop_date()?.to_string()).await?;
    choose(driver, "#location_id", &basics.location.to_string()).await?;
    choose(driver, "#charity_id", &basics.charity.to_string()).await?;
    click(
        driver,
        "form[action=\"/staff/bake-days/new\"] button[type=submit]",
    )
    .await?;
    let day: i64 = wait_for("the Bread Drops list naming the new one", || async {
        let url = driver.current_url().await?;
        Ok((url.path() == "/staff/bake-days")
            .then(|| {
                url.query_pairs()
                    .find(|(key, _)| key == "toegevoegd")
                    .and_then(|(_, id)| id.parse().ok())
            })
            .flatten())
    })
    .await?;
    let date = ctx.i18n.long_date(seed::drop_date()?);
    wait_for_text(driver, ".table-row-saved", &date).await?;
    axe(ctx, driver, "Bread Drops after a save").await?;
    let page = format!("/staff/bake-days/{day}");
    driver.goto(ctx.url(&page)).await?;
    wait_for_text(driver, "main", &ctx.t("day-status-draft")?).await?;
    axe(ctx, driver, "bake day").await?;

    choose(driver, "#product_id", &basics.product.to_string()).await?;
    fill(driver, "#max_qty", "12").await?;
    click(driver, "button[form=\"aanbod-nieuw\"]").await?;
    max_qty_is(ctx, day, basics.product, 12).await?;

    let row = format!("#aanbod-{}", basics.product);
    let limit = format!("#max_qty_{}", basics.product);
    driver.goto(ctx.url(&page)).await?;
    wait_for_text(driver, &row, &basics.product_name).await?;
    set_value(driver, &limit, "15").await?;
    driver
        .find(By::Css(&limit))
        .await?
        .send_keys(Key::Enter)
        .await?;
    max_qty_is(ctx, day, basics.product, 15).await?;
    wait_for_text(driver, "#status", &ctx.t("saved")?).await?;
    axe(ctx, driver, "bake day range").await?;

    driver.goto(ctx.url(&page)).await?;
    click(driver, "button[data-dialog-open=\"day-open\"]").await?;
    wait_for("the open dialog", || async {
        Ok(driver.find(By::Css("dialog#day-open[open]")).await.ok())
    })
    .await?;
    click(driver, "dialog#day-open button[type=submit]").await?;
    wait_for("the bake day to open", || async {
        let status: String = sqlx::query_scalar("SELECT status FROM bake_day WHERE id = $1")
            .bind(day)
            .fetch_one(&ctx.pool)
            .await?;
        Ok((status == "open").then_some(()))
    })
    .await?;
    driver.goto(ctx.url(&page)).await?;
    wait_for_text(driver, "main", &ctx.t("day-status-open")?).await?;

    driver.goto(ctx.url(&format!("/drops/{day}"))).await?;
    wait_for_text(driver, ".product-name", &basics.product_name).await
}

async fn customer_login(ctx: &Ctx, driver: &WebDriver) -> Result<(), Error> {
    let email = seed::customer_email("login");
    seed::customer(&ctx.pool, &email).await?;

    driver.goto(ctx.url("/inloggen")).await?;
    wait_for_text(driver, "h1", &ctx.t("account-login-title")?).await?;
    fill(driver, "#email", &email).await?;
    click(driver, "form[action=\"/inloggen\"] button[type=submit]").await?;
    wait_for_text(driver, "h1", &ctx.t("account-check-title")?).await?;

    let body: String = wait_for("the login link email", || async {
        Ok(sqlx::query_scalar(
            "SELECT text_body FROM email_outbox WHERE kind = 'login_link' AND to_address = $1",
        )
        .bind(&email)
        .fetch_optional(&ctx.pool)
        .await?)
    })
    .await?;
    driver
        .goto(link_in(&ctx.server, &body, "/inloggen/")?)
        .await?;
    wait_for_text(driver, "h1", &ctx.t("account-link-title")?).await?;
    click(driver, "main form button[type=submit]").await?;

    wait_for_text(driver, "h1", &ctx.t("account-orders-title")?).await?;
    wait_for_text(
        driver,
        "main",
        &ctx.t_with("account-orders-signed-in-as", "email", email.clone())?,
    )
    .await?;
    axe(ctx, driver, "Mijn bestellingen").await?;

    click(driver, "form[action=\"/uitloggen\"] button[type=submit]").await?;
    wait_for("the home page", || async {
        Ok((path(driver).await? == "/").then_some(()))
    })
    .await?;
    driver.goto(ctx.url("/mijn-bestellingen")).await?;
    wait_for_text(driver, "h1", &ctx.t("account-login-title")?).await
}

async fn choose_charity(ctx: &Ctx, driver: &WebDriver) -> Result<(), Error> {
    let drop = seed::open_drop(&ctx.pool).await?;
    let other = seed::charity(
        &mut *ctx.pool.acquire().await?,
        &format!("Ander doel {}", seed::unique()),
    )
    .await?;
    let email = seed::customer_email("doel");
    let Paid { order, .. } =
        order_and_pay(ctx, driver, &drop, &email, Some("bewaren"), false).await?;
    let (_, _, body) = confirmation(ctx, order).await?;

    driver
        .goto(link_in(&ctx.server, &body, "/donatie/kies/")?)
        .await?;
    wait_for_text(driver, "h1", &ctx.t("donation-choose-title")?).await?;
    choose(driver, "#goed-doel", &other.to_string()).await?;
    click(driver, "main form button[type=submit]").await?;
    wait_for_text(driver, "h1", &ctx.t("donation-chosen-title")?).await?;

    let (charity, status): (Option<i64>, String) =
        sqlx::query_as("SELECT charity_id, status FROM donation_allocation WHERE order_id = $1")
            .bind(order)
            .fetch_one(&ctx.pool)
            .await?;
    if charity != Some(other) || status != "earmarked" {
        return Err(format!(
            "the donation should go to charity {other}, it has {charity:?} ({status})"
        )
        .into());
    }
    Ok(())
}

async fn sick_day_cancel(ctx: &Ctx, driver: &WebDriver) -> Result<(), Error> {
    let drop = seed::open_drop(&ctx.pool).await?;
    let email = seed::customer_email("ziek");
    let Paid { order, payment } = order_and_pay(ctx, driver, &drop, &email, None, false).await?;

    staff_sign_in(ctx, driver, drop.owner).await?;
    let page = format!("/staff/bake-days/{}/cancel", drop.day);
    driver.goto(ctx.url(&page)).await?;
    fill(driver, "#bericht", "Sorry, de bakker is ziek.").await?;
    click(
        driver,
        &format!("form[action=\"{page}\"] button[type=submit]"),
    )
    .await?;
    wait_for_text(driver, "main", &ctx.t("staff-sick-day-cancelled")?).await?;
    wait_for_text(driver, "main", &ctx.t("staff-sick-day-all-done")?).await?;

    let refunds = ctx
        .mollie
        .payment(&payment.id)
        .map(|p| p.refunds)
        .unwrap_or_default();
    if refunds != ["9.00"] {
        return Err(format!("Mollie should have one refund of 9.00, it has {refunds:?}").into());
    }
    let (to, body): (String, String) = wait_for("the sick-day email", || async {
        Ok(sqlx::query_as(
            "SELECT to_address, text_body FROM email_outbox
             WHERE kind = 'sick_day_cancelled' AND dedupe_key = $1",
        )
        .bind(format!("sick-day:{order}"))
        .fetch_optional(&ctx.pool)
        .await?)
    })
    .await?;
    if to != email || !body.contains("Sorry, de bakker is ziek.") {
        return Err(format!("the sick-day email to {to} lacks the message:\n{body}").into());
    }
    let after = ctx.order_status(order).await?;
    if after != OrderStatus::Refunded && after != OrderStatus::Cancelled {
        return Err(format!("after the cancel the order is {after:?}").into());
    }
    Ok(())
}

async fn pickup_list(ctx: &Ctx, driver: &WebDriver) -> Result<(), Error> {
    let drop = seed::open_drop(&ctx.pool).await?;
    let email = seed::customer_email("ophalen");
    let Paid { order, .. } = order_and_pay(ctx, driver, &drop, &email, None, false).await?;

    staff_sign_in(ctx, driver, drop.owner).await?;
    driver
        .goto(ctx.url(&format!("/staff/bake-days/{}/pickup-list", drop.day)))
        .await?;
    let card = format!("article#bestelling-{order}");
    wait_for_text(driver, &card, CUSTOMER_NAME).await?;
    axe(ctx, driver, "pickup list").await?;

    let wait_status = |want: OrderStatus| async move {
        wait_for(&format!("the order to be {want:?}"), || async {
            Ok((ctx.order_status(order).await? == want).then_some(()))
        })
        .await
    };
    click(driver, &format!("{card} form button[type=submit]")).await?;
    wait_status(OrderStatus::PickedUp).await?;
    wait_for_text(driver, &card, &ctx.t("pickup-list-status-picked-up")?).await?;
    wait_for_text(driver, &card, &ctx.t("lists-undo")?).await?;

    click(driver, &format!("{card} form button[type=submit]")).await?;
    wait_status(OrderStatus::Paid).await?;
    wait_for_text(driver, &card, &ctx.t("pickup-list-status-paid")?).await
}

async fn gallery_is_accessible(ctx: &Ctx, driver: &WebDriver) -> Result<(), Error> {
    driver.goto(ctx.url("/gallery")).await?;
    wait_for_text(driver, "h1", &ctx.t("gallery-title")?).await?;
    axe(ctx, driver, "gallery").await
}

/// On a phone: Enter saves a percentage and the dough weight follows, Escape puts a cell
/// back, and a line is added by typing its name (F14). axe-core checks both modes.
async fn recipe_editor(ctx: &Ctx, driver: &WebDriver) -> Result<(), Error> {
    let recipe = seed::recipe(&ctx.pool).await?;
    let stored = async |position: i32| -> Result<i32, Error> {
        Ok(sqlx::query_scalar(
            "SELECT bakers_pct_x1000 FROM recipe_line
             WHERE recipe_version_id = $1 AND position = $2",
        )
        .bind(recipe.version)
        .bind(position)
        .fetch_one(&ctx.pool)
        .await?)
    };
    staff_sign_in(ctx, driver, recipe.owner).await?;
    driver
        .goto(ctx.url(&format!("{}/edit?gewicht=1670", recipe.path)))
        .await?;
    wait_for_text(driver, "#receptuur tfoot", "1.670").await?;
    axe(ctx, driver, "recipe edit").await?;

    // Typing fires change events on its own; a value set by script does not.
    set_value(driver, "#pct-1", "70").await?;
    driver
        .find(By::Css("#pct-1"))
        .await?
        .send_keys(Key::Enter)
        .await?;
    wait_for_text(driver, "#receptuur tfoot", "1.720").await?;
    if stored(1).await? != 70_000 {
        return Err("Enter did not save the water percentage".into());
    }

    let salt = driver.find(By::Css("#pct-2")).await?;
    salt.click().await?;
    salt.send_keys("9").await?;
    salt.send_keys(Key::Escape).await?;
    salt.send_keys(Key::Tab).await?;
    let value = driver.find(By::Css("#pct-2")).await?.prop("value").await?;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    if value.as_deref() != Some("2") || stored(2).await? != 2_000 {
        return Err(format!("Escape left {value:?} in the salt cell").into());
    }

    fill(driver, "#add-q", &recipe.seeds_name).await?;
    let suggestion = format!("#suggesties button[value=\"i{}\"]", recipe.seeds);
    wait_for("the type-ahead suggestion", || async {
        Ok(driver.find(By::Css(&suggestion)).await.ok().map(|_| ()))
    })
    .await?;
    fill(driver, "#add-pct", "5").await?;
    click(driver, &suggestion).await?;
    wait_for_text(driver, "#receptuur tfoot", "1.770").await?;

    click(driver, "#werkwijze input[value=\"steps\"] + button").await?;
    fill(driver, "#stap-nieuw-naam", "Autolyse").await?;
    fill(driver, "#stap-nieuw-duur", "30").await?;
    click(driver, "#werkwijze .method-add button[type=\"submit\"]").await?;
    wait_for("the saved step", || async {
        Ok(driver.find(By::Css("#stap-0-naam")).await.ok().map(|_| ()))
    })
    .await?;
    axe(ctx, driver, "recipe steps edit").await?;

    set_value(driver, "#stap-0-duur", "45").await?;
    driver
        .find(By::Css("#stap-0-duur"))
        .await?
        .send_keys(Key::Enter)
        .await?;
    wait_for("the duration saved from its cell", || async {
        let minutes: Option<i32> = sqlx::query_scalar(
            "SELECT duration_min FROM recipe_step WHERE recipe_version_id = $1 AND position = 0",
        )
        .bind(recipe.version)
        .fetch_one(&ctx.pool)
        .await?;
        Ok((minutes == Some(45)).then_some(()))
    })
    .await?;

    driver
        .goto(ctx.url(&format!("{}?gewicht=1770&start=21:00", recipe.path)))
        .await?;
    wait_for_text(driver, "#receptuur tfoot", "1.770").await?;
    wait_for_text(driver, "#werkwijze .method-end", "21:45").await?;
    axe(ctx, driver, "recipe").await
}

/// Runs axe-core's WCAG 2.2 AA rules on the current page; any violation fails the flow.
async fn axe(ctx: &Ctx, driver: &WebDriver, page: &str) -> Result<(), Error> {
    let axe = std::fs::read_to_string(&ctx.axe_js)
        .map_err(|err| format!("{}: {err}", ctx.axe_js.display()))?;
    driver.execute(axe, Vec::new()).await?;
    let script = format!(
        "const done = arguments[arguments.length - 1];
         axe.run(document, {{ runOnly: {{ type: 'tag', values: {AXE_TAGS} }} }})
           .then(result => done({{ violations: result.violations.map(v => ({{
               id: v.id, impact: v.impact, help: v.help,
               nodes: v.nodes.map(n => n.target.join(' ')) }})) }}))
           .catch(err => done({{ error: String(err) }}));"
    );
    let result = driver.execute_async(script, Vec::new()).await?;
    let result = result.json();
    if let Some(error) = result.get("error") {
        return Err(format!("axe-core failed on the {page} page: {error}").into());
    }
    let violations = result
        .get("violations")
        .and_then(Value::as_array)
        .ok_or("axe-core returned no violations list")?;
    if violations.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} axe-core violations on the {page} page:\n{}",
            violations.len(),
            serde_json::to_string_pretty(violations)?
        )
        .into())
    }
}

/// The Mollie checkout URL the browser tried to open, also from Firefox's error page.
fn checkout_url(current: &str) -> Option<String> {
    let url = reqwest::Url::parse(current).ok()?;
    let target = if url.scheme() == "about" {
        url.query_pairs()
            .find(|(key, _)| key == "u")
            .map(|(_, value)| value.into_owned())?
    } else {
        url.to_string()
    };
    target.starts_with(CHECKOUT_BASE).then_some(target)
}

/// Waits until the range of `day` holds `product` with a maximum of `max`.
async fn max_qty_is(ctx: &Ctx, day: i64, product: i64, max: i32) -> Result<(), Error> {
    wait_for(&format!("a maximum of {max}"), || async {
        Ok(sqlx::query_scalar::<_, Option<i32>>(
            "SELECT max_qty FROM bake_day_product WHERE bake_day_id = $1 AND product_id = $2",
        )
        .bind(day)
        .bind(product)
        .fetch_optional(&ctx.pool)
        .await?
        .flatten()
        .filter(|found| *found == max)
        .map(|_| ()))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkout_url_reads_the_page_or_the_error_page() {
        let url = format!("{CHECKOUT_BASE}tr_e2e1");
        assert_eq!(checkout_url(&url), Some(url.clone()));
        assert_eq!(
            checkout_url(
                "about:neterror?e=connectionFailure&u=https%3A//www.mollie.com/checkout/select-method/tr_e2e1&c=UTF-8"
            ),
            Some(url)
        );
        assert_eq!(
            checkout_url("http://127.0.0.1:8080/drops/1/bestellen"),
            None
        );
        assert_eq!(checkout_url("about:neterror?e=dnsNotFound"), None);
    }

    #[test]
    fn link_in_finds_the_link_and_moves_it_to_the_server() {
        let text = "Hoi,\n\nLog in via https://bvw.test/inloggen/abc_12-.\nTot snel!";
        assert_eq!(
            link_in("http://127.0.0.1:8080", text, "/inloggen/").unwrap(),
            "http://127.0.0.1:8080/inloggen/abc_12-"
        );
        assert!(link_in("http://127.0.0.1:8080", text, "/donatie/kies/").is_err());
    }
}
