use std::future::Future;
use std::time::Duration;

use thirtyfour::common::capabilities::firefox::FirefoxPreferences;
use thirtyfour::prelude::*;

use crate::Error;

const WAIT: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(200);

/// Headless Firefox at laptop size. `www.mollie.com` resolves to localhost, so the hosted
/// checkout page fails to load instead of reaching the internet.
///
/// # Errors
/// When the WebDriver does not answer or refuses the session.
pub async fn firefox(webdriver: &str) -> Result<WebDriver, Error> {
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
    driver.set_window_rect(0, 0, 1280, 900).await?;
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
        let text = element.text().await?.to_lowercase();
        Ok(text.contains(&needle.to_lowercase()).then_some(()))
    })
    .await
}
