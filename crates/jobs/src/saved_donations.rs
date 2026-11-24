//! Saved donations nobody chose a charity for (F4d): each tick moves those past their date
//! to the next drop's charity and queues an email to the customer, one donation per
//! transaction.

use domain::{Clock, DonationAllocationId};
use orders::donations::{self, Expired};
use sqlx::{Acquire, PgConnection};

use crate::order_emails::{self, EmailError, OrderEmails};

/// Donations moved per tick at most; the rest wait for the next minute.
pub const MOVE_BATCH: usize = 100;

/// What one run did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MoveReport {
    pub moved: Vec<DonationAllocationId>,
    /// A donation is past its date but no coming drop has an active charity.
    pub waiting: bool,
}

/// Moves saved donations past their date. Safe to repeat: a moved donation is no longer
/// saved, and the email has a dedupe key. An email that cannot be composed (a bad
/// address) is logged and the donation moves anyway; any other error rolls back that
/// donation and stops the run.
///
/// # Errors
/// [`EmailError`] from the database or the outbox, or
/// [`EmailError::CompanyIncomplete`].
pub async fn tick(
    conn: &mut PgConnection,
    clock: &dyn Clock,
    emails: &OrderEmails,
) -> Result<MoveReport, EmailError> {
    let mut report = MoveReport::default();
    for _ in 0..MOVE_BATCH {
        let mut tx = conn.begin().await.map_err(orders::Error::from)?;
        let moved = match donations::move_next_expired(&mut tx, clock.now()).await? {
            Expired::Nothing => break,
            Expired::NoDrop => {
                report.waiting = true;
                break;
            }
            Expired::Moved(moved) => moved,
        };
        if moved.notify {
            let mut email_tx = tx.begin().await.map_err(orders::Error::from)?;
            match order_emails::enqueue_donation_moved(&mut email_tx, clock, emails, &moved).await {
                Ok(_) => email_tx.commit().await.map_err(orders::Error::from)?,
                Err(EmailError::Compose(error)) => {
                    tracing::error!(
                        allocation = moved.allocation.get(),
                        %error,
                        "saved donation moved without its email"
                    );
                    email_tx.rollback().await.map_err(orders::Error::from)?;
                }
                Err(error) => return Err(error),
            }
        }
        tx.commit().await.map_err(orders::Error::from)?;
        report.moved.push(moved.allocation);
    }
    Ok(report)
}
