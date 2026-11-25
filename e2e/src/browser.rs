use std::future::Future;
use std::time::Duration;

use thirtyfour::common::capabilities::firefox::FirefoxPreferences;
use thirtyfour::prelude::*;

use crate::Error;

const WAIT: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(200);

/// A laptop window.
pub const LAPTOP: (u32, u32) = (1280, 900);
/// An iPhone-sized window, for the screens the spec designs for the phone first.
pub const PHONE: (u32, u32) = (390, 844);

/// Headless Firefox with a window of `size`. `www.mollie.com` resolves to localhost, so
/// the hosted checkout page fails to load instead of reaching the internet.
///
/// # Errors
/// When the WebDriver does not answer or refuses the session.
pub async fn firefox(webdriver: &str, size: (u32, u32)) -> Result<WebDriver, Error> {
    let mut caps = DesiredCapabilities::firefox();
    caps.set_headless()?;
    let mut prefs = FirefoxPreferences::new();
    prefs.set("network.dns.localDomains", "www.mollie.com")?;
    caps.set_preferences(prefs)?;
    let driver = WebDriver::new(webdriver, caps).await?;
    driver
        .update_timeouts(TimeoutConfiguration::new(
            Some(Duration::from_secs(60)),
            Some(Duration::from_secs(30)),
            Some(Duration::ZERO),
        ))
        .await?;
    driver.set_window_rect(0, 0, size.0, size.1).await?;
    Ok(driver)
}

/// Polls `check` until it returns `Some`, for at most 15 seconds.
///
/// # Errors
/// `what` when time runs out, or the first error `check` returns.
pub async fn wait_for<T, F, Fut>(what: &str, mut check: F) -> Result<T, Error>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Option<T>, Error>>,
{
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        if let Some(value) = check().await? {
            return Ok(value);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("timed out waiting for {what}").into());
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Waits until the element at `css` exists and its text contains `needle`, ignoring case:
/// headings are upper case through CSS, and WebDriver returns the rendered text.
///
/// # Errors
/// When it does not appear in time.
pub async fn wait_for_text(driver: &WebDriver, css: &str, needle: &str) -> Result<(), Error> {
    wait_for(&format!("{needle:?} in {css}"), || async {
        let Ok(element) = driver.find(By::Css(css)).await else {
            return Ok(None);
        };
        // HTMX swaps can replace the element between find and text: poll again.
        let Ok(text) = element.text().await else {
            return Ok(None);
        };
        let text = text.to_lowercase();
        Ok(text.contains(&needle.to_lowercase()).then_some(()))
    })
    .await
}

/// Clicks the element at `css`.
///
/// # Errors
/// When there is no such element or it cannot be clicked.
pub async fn click(driver: &WebDriver, css: &str) -> Result<(), Error> {
    driver
        .find(By::Css(css))
        .await
        .map_err(|err| format!("{css}: {err}"))?
        .click()
        .await?;
    Ok(())
}

/// Replaces the text of the field at `css`.
///
/// # Errors
/// When there is no such field.
pub async fn fill(driver: &WebDriver, css: &str, text: &str) -> Result<(), Error> {
    let field = driver
        .find(By::Css(css))
        .await
        .map_err(|err| format!("{css}: {err}"))?;
    field.clear().await?;
    field.send_keys(text).await?;
    Ok(())
}

/// Picks the option with `value` in the select at `css`.
///
/// # Errors
/// When the select has no such option.
pub async fn choose(driver: &WebDriver, css: &str, value: &str) -> Result<(), Error> {
    click(driver, &format!("{css} option[value=\"{value}\"]")).await
}

/// Sets the value of the field at `css` by script: typing into a date field depends on
/// the browser's locale.
///
/// # Errors
/// When the script fails, for example because there is no such field.
pub async fn set_value(driver: &WebDriver, css: &str, value: &str) -> Result<(), Error> {
    driver
        .execute(
            "document.querySelector(arguments[0]).value = arguments[1];",
            vec![serde_json::json!(css), serde_json::json!(value)],
        )
        .await?;
    Ok(())
}

/// The path of the current page, such as `/staff/bake-days/7`.
///
/// # Errors
/// When the WebDriver does not answer.
pub async fn path(driver: &WebDriver) -> Result<String, Error> {
    Ok(driver.current_url().await?.path().to_owned())
}
