use std::path::PathBuf;

use domain::OrderId;
use i18n::{FluentArgs, Translator};
use serde_json::Value;
use sqlx::PgPool;
use thirtyfour::prelude::*;

use crate::browser::{firefox, wait_for, wait_for_text};
use crate::mollie::{CHECKOUT_BASE, MockMollie, on_server};
use crate::{Error, seed};

const CUSTOMER_NAME: &str = "Test Klant";
const CUSTOMER_EMAIL: &str = "klant@example.org";

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    BuyALoaf,
    GalleryAxe,
}

impl Flow {
    pub const ALL: [Self; 2] = [Self::BuyALoaf, Self::GalleryAxe];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::BuyALoaf => "buy a loaf, pay and get the confirmation (F4, F5, F9)",
            Self::GalleryAxe => "the gallery passes axe-core (WCAG 2.2 AA)",
        }
    }
}

/// Runs `flow` in a fresh browser session, which is closed whatever the outcome.
///
/// # Errors
/// What the flow or the WebDriver reports.
pub async fn run(ctx: &Ctx, flow: Flow) -> Result<(), Error> {
    let driver = firefox(&ctx.webdriver).await?;
    let result = match flow {
        Flow::BuyALoaf => buy_a_loaf(ctx, &driver).await,
        Flow::GalleryAxe => gallery_is_accessible(ctx, &driver).await,
    };
    let quit = driver.quit().await;
    result?;
    quit?;
    Ok(())
}

async fn buy_a_loaf(ctx: &Ctx, driver: &WebDriver) -> Result<(), Error> {
    let drop = seed::open_drop(&ctx.pool).await?;
    driver
        .goto(ctx.url(&format!("/drops/{}", drop.day)))
        .await?;
    wait_for_text(driver, ".product-name", &drop.product_name).await?;

    let qty = driver
        .find(By::Css(format!("#aantal-{}", drop.product)))
        .await?;
    qty.clear().await?;
    qty.send_keys("2").await?;
    driver
        .find(By::Css("form.product-order button[type=submit]"))
        .await?
        .click()
        .await?;
    wait_for_text(driver, "#mandje", &ctx.t("cart-added")?).await?;
    driver
        .find(By::Css("#mandje a.button-primary"))
        .await?
        .click()
        .await?;

    wait_for_text(driver, "h1", &ctx.t("checkout-title")?).await?;
    driver
        .find(By::Css("#naam"))
        .await?
        .send_keys(CUSTOMER_NAME)
        .await?;
    driver
        .find(By::Css("#email"))
        .await?
        .send_keys(CUSTOMER_EMAIL)
        .await?;
    driver
        .find(By::Css("label[for=ophaalmoment-1]"))
        .await?
        .click()
        .await?;
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
    let (to, subject, body): (String, String, String) =
        wait_for("the confirmation email", || async {
            Ok(sqlx::query_as(
                "SELECT to_address, subject, text_body FROM email_outbox
             WHERE kind = 'order_confirmation' AND dedupe_key = $1",
            )
            .bind(format!("order-confirmation:{order}"))
            .fetch_optional(&ctx.pool)
            .await?)
        })
        .await?;
    // Checked before the status page opens: that page asks Mollie itself and would settle
    // the payment even if the webhook did not.
    let placed = orders::order::get(&mut *ctx.pool.acquire().await?, OrderId::new(order)).await?;
    if placed.status != orders::OrderStatus::Paid {
        return Err(format!("after the webhook the order is {:?}", placed.status).into());
    }
    let number = placed.number;
    let checks = [
        (to == CUSTOMER_EMAIL, "is sent to the customer"),
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
    Ok(())
}

async fn gallery_is_accessible(ctx: &Ctx, driver: &WebDriver) -> Result<(), Error> {
    let axe = std::fs::read_to_string(&ctx.axe_js)
        .map_err(|err| format!("{}: {err}", ctx.axe_js.display()))?;
    driver.goto(ctx.url("/gallery")).await?;
    wait_for_text(driver, "h1", &ctx.t("gallery-title")?).await?;
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
        return Err(format!("axe-core failed: {error}").into());
    }
    let violations = result
        .get("violations")
        .and_then(Value::as_array)
        .ok_or("axe-core returned no violations list")?;
    if violations.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} axe-core violations:\n{}",
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
}
