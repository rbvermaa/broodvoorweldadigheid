//! The emails of F2, F9 and F9a, built from plain inputs so no order or subscriber code is
//! needed here. All text comes from `locales/nl/email.ftl`.

use std::str::FromStr;

use domain::{Line, Money, MoneyError, VatRate, VatTotals};
use i18n::{Amsterdam, FluentArgs, I18nError, Translator};
use jiff::Timestamp;
use jiff::civil::Date;

use crate::content::{Block, Content, Fact, Lines, Link, RenderError, Row};
use crate::email::{Email, EmailKind};

/// The trade name, address and KvK number every email carries (Compliance).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Company {
    pub name: String,
    pub address: String,
    pub kvk: String,
}

impl Company {
    /// The company settings row. Address and KvK number are empty until staff fill them in.
    ///
    /// # Errors
    /// When the database fails.
    pub async fn load(conn: &mut sqlx::PgConnection) -> Result<Self, sqlx::Error> {
        let row = sqlx::query!(
            r#"SELECT name, coalesce(address, '') AS "address!", coalesce(kvk, '') AS "kvk!"
            FROM company WHERE id = 1"#
        )
        .fetch_one(conn)
        .await?;
        Ok(Self {
            name: row.name,
            address: row.address,
            kvk: row.kvk,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipient {
    pub email: String,
    pub name: Option<String>,
}

/// An ophaalmoment at a pickup location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pickup {
    pub starts: Timestamp,
    pub ends: Timestamp,
    pub location: String,
    pub address: String,
}

/// A line as sold: the unit price includes BTW and is the price paid, which for
/// pay what you want is the customer's own price.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderLine {
    pub product: String,
    pub qty: u32,
    pub unit_price: Money,
    pub vat_rate: VatRate,
    pub pay_what_you_want: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub qty: u32,
    pub product: String,
}

/// Where the money of an order goes (F4c, F4d).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Donation {
    /// A drop without a charity: no donation text at all.
    NormalSale,
    Charity {
        name: String,
    },
    Saved {
        choose_url: String,
        choose_by: Date,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderConfirmation {
    pub to: Recipient,
    pub order_number: String,
    pub drop_date: Date,
    pub pickup: Pickup,
    pub lines: Vec<OrderLine>,
    pub donation: Donation,
    /// The order's status page.
    pub status_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickupReminder {
    pub to: Recipient,
    pub order_number: String,
    pub pickup: Pickup,
    pub items: Vec<Item>,
}

/// F1, F4a: staff moved the ophaalmoment of an order; `pickup` is the new one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickupMoved {
    pub to: Recipient,
    pub order_number: String,
    pub pickup: Pickup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderCancelled {
    pub to: Recipient,
    pub order_number: String,
    pub drop_date: Date,
    /// `None` when nothing was paid, such as an unpaid or on-account order.
    pub refund: Option<Money>,
}

/// F2: the baker cancels a whole drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SickDayCancelled {
    pub to: Recipient,
    pub order_number: String,
    pub drop_date: Date,
    pub message: String,
    pub refund: Option<Money>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropAlertKind {
    MenuPublished,
    OrderingOpen,
}

/// F9a: sent only to confirmed subscribers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropAlert {
    pub to: String,
    pub kind: DropAlertKind,
    pub drop_date: Date,
    pub pickup: Pickup,
    pub deadline: Option<Timestamp>,
    /// `None` for a normal sale.
    pub charity: Option<String>,
    pub menu: Vec<String>,
    pub shop_url: String,
    /// Opens the unsubscribe page and accepts the RFC 8058 one-click POST.
    pub unsubscribe_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionConfirmation {
    pub to: String,
    pub confirm_url: String,
}

/// A customer's magic link to sign in (spec, Security; ADR 0029).
#[derive(Clone, PartialEq, Eq)]
pub struct LoginLink {
    pub to: String,
    pub login_url: String,
    pub valid_minutes: i64,
}

impl std::fmt::Debug for LoginLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginLink")
            .field("to", &self.to)
            .field("login_url", &"<redacted>")
            .field("valid_minutes", &self.valid_minutes)
            .finish()
    }
}

/// A saved donation nobody chose a charity for went to the next drop's charity (F4d).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DonationMoved {
    pub to: Recipient,
    pub order_number: String,
    pub amount: Money,
    pub charity: String,
    /// The drop whose charity it went to.
    pub drop_date: Date,
    /// The order's status page.
    pub status_url: String,
}

/// F4f: a message from the contact form, sent to the bakery. Replies go to `email`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactMessage {
    pub to: String,
    pub name: String,
    pub email: String,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ComposeError {
    #[error(transparent)]
    I18n(#[from] I18nError),
    #[error(transparent)]
    Money(#[from] MoneyError),
    #[error(transparent)]
    Render(#[from] RenderError),
    #[error("the recipient is not an email address")]
    Address,
    #[error("a link is not a plain https URL")]
    Url,
    #[error("the company name, address and KvK number must all be filled in")]
    Company,
    #[error("an order confirmation needs at least one line")]
    NoLines,
    #[error("a refund must be more than zero, got {0} cents")]
    Refund(i64),
    #[error("the contact name or message is empty or holds control characters")]
    Contact,
}

/// The parts that differ per email; [`Composer::email`] adds the frame.
struct Parts {
    subject: String,
    heading: String,
    greeting: String,
    blocks: Vec<Block>,
    unsubscribe: Option<Link>,
}

pub struct Composer<'a> {
    i18n: &'a Translator,
    company: &'a Company,
    zone: Amsterdam,
}

impl<'a> Composer<'a> {
    #[must_use]
    pub fn new(i18n: &'a Translator, company: &'a Company) -> Self {
        Self {
            i18n,
            company,
            zone: Amsterdam::new(),
        }
    }

    /// # Errors
    /// [`ComposeError`] for a bad address or URL, no lines, an amount that overflows, or a
    /// missing Fluent message.
    pub fn order_confirmation(&self, order: &OrderConfirmation) -> Result<Email, ComposeError> {
        if order.lines.is_empty() {
            return Err(ComposeError::NoLines);
        }
        let number = order.order_number.as_str();
        let mut blocks = vec![
            Block::Paragraph(self.t("email-order-confirmation-intro", &[])?),
            Block::Facts(vec![
                self.fact("email-label-order", number.to_owned())?,
                self.fact("email-label-drop", self.i18n.long_date(order.drop_date))?,
                self.fact("email-label-pickup", self.moment(&order.pickup)?)?,
                self.fact("email-label-location", self.place(&order.pickup)?)?,
            ]),
            Block::Lines(self.lines(&order.lines)?),
        ];
        match &order.donation {
            Donation::NormalSale => {}
            Donation::Charity { name } => blocks.push(Block::Paragraph(self.t(
                "email-order-confirmation-charity",
                &[("charity", name.clone())],
            )?)),
            Donation::Saved {
                choose_url,
                choose_by,
            } => {
                blocks.push(Block::Paragraph(self.t(
                    "email-order-confirmation-saved",
                    &[("date", self.i18n.long_date(*choose_by))],
                )?));
                blocks.push(Block::Button(Link {
                    label: self.t("email-order-confirmation-choose", &[])?,
                    url: https(choose_url)?,
                }));
            }
        }
        blocks.push(Block::Button(Link {
            label: self.t("email-order-confirmation-status", &[])?,
            url: https(&order.status_url)?,
        }));
        blocks.push(Block::Paragraph(self.t("email-questions", &[])?));
        let parts = Parts {
            subject: self.t(
                "email-order-confirmation-subject",
                &[("number", number.to_owned())],
            )?,
            heading: self.t("email-order-confirmation-heading", &[])?,
            greeting: self.greeting(order.to.name.as_deref())?,
            blocks,
            unsubscribe: None,
        };
        self.email(EmailKind::OrderConfirmation, &order.to.email, parts)
    }

    /// # Errors
    /// See [`Composer::order_confirmation`].
    pub fn pickup_reminder(&self, reminder: &PickupReminder) -> Result<Email, ComposeError> {
        let (day, _) = self.day_and_range(&reminder.pickup)?;
        let number = reminder.order_number.as_str();
        let items = reminder
            .items
            .iter()
            .map(|item| {
                self.t(
                    "email-item",
                    &[
                        ("qty", item.qty.to_string()),
                        ("product", item.product.clone()),
                    ],
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut blocks = vec![
            Block::Paragraph(self.t(
                "email-pickup-reminder-intro",
                &[
                    ("day", day),
                    ("start", self.zone.time(reminder.pickup.starts)),
                    ("end", self.zone.time(reminder.pickup.ends)),
                ],
            )?),
            Block::Facts(vec![
                self.fact("email-label-order", number.to_owned())?,
                self.fact("email-label-pickup", self.moment(&reminder.pickup)?)?,
                self.fact("email-label-location", self.place(&reminder.pickup)?)?,
            ]),
        ];
        if !items.is_empty() {
            blocks.push(Block::Paragraph(
                self.t("email-pickup-reminder-items", &[])?,
            ));
            blocks.push(Block::Items(items));
        }
        let parts = Parts {
            subject: self.t(
                "email-pickup-reminder-subject",
                &[("number", number.to_owned())],
            )?,
            heading: self.t("email-pickup-reminder-heading", &[])?,
            greeting: self.greeting(reminder.to.name.as_deref())?,
            blocks,
            unsubscribe: None,
        };
        self.email(EmailKind::PickupReminder, &reminder.to.email, parts)
    }

    /// # Errors
    /// See [`Composer::order_confirmation`].
    pub fn pickup_moved(&self, moved: &PickupMoved) -> Result<Email, ComposeError> {
        let number = moved.order_number.as_str();
        let blocks = vec![
            Block::Paragraph(self.t("email-pickup-moved-intro", &[("number", number.to_owned())])?),
            Block::Facts(vec![
                self.fact("email-label-order", number.to_owned())?,
                self.fact("email-label-pickup", self.moment(&moved.pickup)?)?,
                self.fact("email-label-location", self.place(&moved.pickup)?)?,
            ]),
            Block::Paragraph(self.t("email-pickup-moved-sorry", &[])?),
        ];
        let parts = Parts {
            subject: self.t(
                "email-pickup-moved-subject",
                &[("number", number.to_owned())],
            )?,
            heading: self.t("email-pickup-moved-heading", &[])?,
            greeting: self.greeting(moved.to.name.as_deref())?,
            blocks,
            unsubscribe: None,
        };
        self.email(EmailKind::PickupMoved, &moved.to.email, parts)
    }

    /// # Errors
    /// See [`Composer::order_confirmation`]; also a refund of zero or less.
    pub fn order_cancelled(&self, cancelled: &OrderCancelled) -> Result<Email, ComposeError> {
        let number = cancelled.order_number.as_str();
        let mut blocks = vec![Block::Paragraph(self.t(
            "email-order-cancelled-intro",
            &[
                ("number", number.to_owned()),
                ("date", self.i18n.long_date(cancelled.drop_date)),
            ],
        )?)];
        if let Some(refund) = cancelled.refund {
            blocks.push(Block::Paragraph(self.refund(refund)?));
        }
        blocks.push(Block::Paragraph(self.t("email-questions", &[])?));
        let parts = Parts {
            subject: self.t(
                "email-order-cancelled-subject",
                &[("number", number.to_owned())],
            )?,
            heading: self.t("email-order-cancelled-heading", &[])?,
            greeting: self.greeting(cancelled.to.name.as_deref())?,
            blocks,
            unsubscribe: None,
        };
        self.email(EmailKind::OrderCancelled, &cancelled.to.email, parts)
    }

    /// # Errors
    /// See [`Composer::order_cancelled`].
    pub fn sick_day_cancelled(&self, sick: &SickDayCancelled) -> Result<Email, ComposeError> {
        let date = self.i18n.long_date(sick.drop_date);
        let mut blocks = vec![
            Block::Paragraph(self.t(
                "email-sick-day-intro",
                &[
                    ("date", date.clone()),
                    ("number", sick.order_number.clone()),
                ],
            )?),
            Block::Quote(
                sick.message
                    .trim()
                    .lines()
                    .map(|line| line.trim_end().to_owned())
                    .collect(),
            ),
        ];
        if let Some(refund) = sick.refund {
            blocks.push(Block::Paragraph(self.refund(refund)?));
        }
        blocks.push(Block::Paragraph(self.t("email-sick-day-sorry", &[])?));
        let parts = Parts {
            subject: self.t("email-sick-day-subject", &[("date", date)])?,
            heading: self.t("email-sick-day-heading", &[])?,
            greeting: self.greeting(sick.to.name.as_deref())?,
            blocks,
            unsubscribe: None,
        };
        self.email(EmailKind::SickDayCancelled, &sick.to.email, parts)
    }

    /// # Errors
    /// See [`Composer::order_confirmation`].
    pub fn drop_alert(&self, alert: &DropAlert) -> Result<Email, ComposeError> {
        let date = self.i18n.long_date(alert.drop_date);
        let (subject, heading, intro) = match alert.kind {
            DropAlertKind::MenuPublished => (
                "email-drop-menu-subject",
                "email-drop-menu-heading",
                "email-drop-menu-intro",
            ),
            DropAlertKind::OrderingOpen => (
                "email-drop-open-subject",
                "email-drop-open-heading",
                "email-drop-open-intro",
            ),
        };
        let mut facts = vec![
            self.fact("email-label-pickup", self.moment(&alert.pickup)?)?,
            self.fact("email-label-location", self.place(&alert.pickup)?)?,
        ];
        if let Some(deadline) = alert.deadline {
            let local = self.zone.local(deadline);
            let value = self.t(
                "email-deadline",
                &[
                    ("day", self.i18n.long_date(local.date())),
                    ("time", i18n::time(local.time())),
                ],
            )?;
            facts.push(self.fact("email-label-deadline", value)?);
        }
        let charity = match &alert.charity {
            Some(name) => name.clone(),
            None => self.t("email-normal-sale", &[])?,
        };
        facts.push(self.fact("email-label-charity", charity)?);
        let mut blocks = vec![
            Block::Paragraph(self.t(intro, &[("date", date.clone())])?),
            Block::Facts(facts),
        ];
        if !alert.menu.is_empty() {
            blocks.push(Block::Paragraph(self.t("email-drop-menu", &[])?));
            blocks.push(Block::Items(alert.menu.clone()));
        }
        blocks.push(Block::Button(Link {
            label: self.t("email-drop-shop", &[])?,
            url: https(&alert.shop_url)?,
        }));
        let unsubscribe = https(&alert.unsubscribe_url)?;
        let parts = Parts {
            subject: self.t(subject, &[("date", date)])?,
            heading: self.t(heading, &[])?,
            greeting: self.greeting(None)?,
            blocks,
            unsubscribe: Some(Link {
                label: self.t("email-unsubscribe", &[])?,
                url: unsubscribe.clone(),
            }),
        };
        let mut email = self.email(EmailKind::DropAlert, &alert.to, parts)?;
        email.list_unsubscribe = Some(unsubscribe);
        Ok(email)
    }

    /// # Errors
    /// See [`Composer::order_confirmation`].
    pub fn subscription_confirmation(
        &self,
        subscription: &SubscriptionConfirmation,
    ) -> Result<Email, ComposeError> {
        let parts = Parts {
            subject: self.t("email-subscription-subject", &[])?,
            heading: self.t("email-subscription-heading", &[])?,
            greeting: self.greeting(None)?,
            blocks: vec![
                Block::Paragraph(self.t("email-subscription-intro", &[])?),
                Block::Button(Link {
                    label: self.t("email-subscription-confirm", &[])?,
                    url: https(&subscription.confirm_url)?,
                }),
                Block::Paragraph(self.t("email-subscription-ignore", &[])?),
            ],
            unsubscribe: None,
        };
        self.email(EmailKind::SubscriptionConfirmation, &subscription.to, parts)
    }

    /// # Errors
    /// See [`Composer::order_confirmation`].
    pub fn login_link(&self, link: &LoginLink) -> Result<Email, ComposeError> {
        let parts = Parts {
            subject: self.t("email-login-subject", &[])?,
            heading: self.t("email-login-heading", &[])?,
            greeting: self.greeting(None)?,
            blocks: vec![
                Block::Paragraph(self.t(
                    "email-login-intro",
                    &[("minutes", link.valid_minutes.to_string())],
                )?),
                Block::Button(Link {
                    label: self.t("email-login-button", &[])?,
                    url: https(&link.login_url)?,
                }),
                Block::Paragraph(self.t("email-login-ignore", &[])?),
            ],
            unsubscribe: None,
        };
        self.email(EmailKind::LoginLink, &link.to, parts)
    }

    /// # Errors
    /// See [`Composer::order_confirmation`].
    pub fn donation_moved(&self, moved: &DonationMoved) -> Result<Email, ComposeError> {
        let parts = Parts {
            subject: self.t(
                "email-donation-moved-subject",
                &[("charity", moved.charity.clone())],
            )?,
            heading: self.t("email-donation-moved-heading", &[])?,
            greeting: self.greeting(moved.to.name.as_deref())?,
            blocks: vec![
                Block::Paragraph(self.t(
                    "email-donation-moved-intro",
                    &[
                        ("amount", i18n::money(moved.amount.cents())),
                        ("number", moved.order_number.clone()),
                        ("charity", moved.charity.clone()),
                        ("date", self.i18n.long_date(moved.drop_date)),
                    ],
                )?),
                Block::Button(Link {
                    label: self.t("email-order-confirmation-status", &[])?,
                    url: https(&moved.status_url)?,
                }),
                Block::Paragraph(self.t("email-questions", &[])?),
            ],
            unsubscribe: None,
        };
        self.email(EmailKind::DonationMoved, &moved.to.email, parts)
    }

    /// The last email to a customer whose data was erased (F12). No name: it is gone.
    ///
    /// # Errors
    /// See [`Composer::order_confirmation`].
    pub fn account_deleted(&self, to: &str) -> Result<Email, ComposeError> {
        let parts = Parts {
            subject: self.t("email-account-deleted-subject", &[])?,
            heading: self.t("email-account-deleted-heading", &[])?,
            greeting: self.greeting(None)?,
            blocks: vec![
                Block::Paragraph(self.t("email-account-deleted-intro", &[])?),
                Block::Paragraph(self.t("email-account-deleted-orders", &[])?),
                Block::Paragraph(self.t("email-questions", &[])?),
            ],
            unsubscribe: None,
        };
        self.email(EmailKind::AccountDeleted, to, parts)
    }

    /// To the bakery, with only the company name in the footer; the sender gets no copy.
    ///
    /// # Errors
    /// [`ComposeError::Address`] for a bad recipient or sender address,
    /// [`ComposeError::Contact`] for an empty name or message or a control character in
    /// the name, and [`ComposeError::Company`] without a company name.
    pub fn contact_message(&self, contact: &ContactMessage) -> Result<Email, ComposeError> {
        if !is_address(&contact.email) {
            return Err(ComposeError::Address);
        }
        let name = contact.name.trim();
        if name.is_empty() || contact.name.chars().any(char::is_control) {
            return Err(ComposeError::Contact);
        }
        let message = contact.message.trim();
        if message.is_empty() {
            return Err(ComposeError::Contact);
        }
        let company = self.company.name.trim();
        if company.is_empty() {
            return Err(ComposeError::Company);
        }
        let parts = Parts {
            subject: self.t("email-contact-subject", &[("name", name.to_owned())])?,
            heading: self.t("email-contact-heading", &[])?,
            greeting: self.t("email-contact-greeting", &[])?,
            blocks: vec![
                Block::Paragraph(self.t("email-contact-intro", &[])?),
                Block::Facts(vec![
                    self.fact("email-label-name", name.to_owned())?,
                    self.fact("email-label-email", contact.email.clone())?,
                ]),
                Block::Quote(
                    message
                        .lines()
                        .map(|line| line.trim_end().to_owned())
                        .collect(),
                ),
                Block::Paragraph(self.t("email-contact-reply", &[("name", name.to_owned())])?),
            ],
            unsubscribe: None,
        };
        let mut email = self.frame(
            EmailKind::ContactMessage,
            &contact.to,
            parts,
            company.to_owned(),
            "email-contact-signoff",
        )?;
        email.reply_to = Some(contact.email.clone());
        Ok(email)
    }

    fn email(&self, kind: EmailKind, to: &str, parts: Parts) -> Result<Email, ComposeError> {
        self.frame(kind, to, parts, self.company_line()?, "email-signoff")
    }

    fn frame(
        &self,
        kind: EmailKind,
        to: &str,
        parts: Parts,
        company: String,
        signoff: &str,
    ) -> Result<Email, ComposeError> {
        let to = address(to)?;
        let reason = match parts.unsubscribe {
            Some(_) => Some(self.t("email-drop-reason", &[])?),
            None => None,
        };
        let content = Content {
            subject: parts.subject,
            brand: self.t("app-name", &[])?,
            tagline: self.t("tagline", &[])?,
            heading: parts.heading,
            greeting: parts.greeting,
            blocks: parts.blocks,
            signoff: self.t(signoff, &[])?,
            reason,
            unsubscribe: parts.unsubscribe,
            company,
        };
        Ok(Email {
            kind,
            to,
            html: content.html()?,
            text: content.text(),
            subject: content.subject,
            list_unsubscribe: None,
            reply_to: None,
        })
    }

    fn t(&self, id: &str, args: &[(&str, String)]) -> Result<String, I18nError> {
        if args.is_empty() {
            return self.i18n.t(id, None);
        }
        let mut fluent = FluentArgs::new();
        for (name, value) in args {
            fluent.set(*name, value.clone());
        }
        self.i18n.t(id, Some(&fluent))
    }

    fn fact(&self, label: &str, value: String) -> Result<Fact, I18nError> {
        Ok(Fact {
            label: self.t(label, &[])?,
            value,
        })
    }

    fn greeting(&self, name: Option<&str>) -> Result<String, I18nError> {
        match name.map(str::trim).filter(|name| !name.is_empty()) {
            Some(name) => self.t("email-greeting", &[("name", name.to_owned())]),
            None => self.t("email-greeting-anonymous", &[]),
        }
    }

    fn day_and_range(&self, pickup: &Pickup) -> Result<(String, String), I18nError> {
        let day = self.i18n.long_date(self.zone.date(pickup.starts));
        let range = self.zone.time_range(pickup.starts, pickup.ends)?;
        Ok((day, range))
    }

    fn moment(&self, pickup: &Pickup) -> Result<String, I18nError> {
        let (day, range) = self.day_and_range(pickup)?;
        self.t("email-moment", &[("day", day), ("range", range)])
    }

    fn place(&self, pickup: &Pickup) -> Result<String, I18nError> {
        self.t(
            "email-place",
            &[
                ("name", pickup.location.clone()),
                ("address", pickup.address.clone()),
            ],
        )
    }

    fn refund(&self, amount: Money) -> Result<String, ComposeError> {
        if amount <= Money::ZERO {
            return Err(ComposeError::Refund(amount.cents()));
        }
        Ok(self.t("email-refund", &[("amount", i18n::money(amount.cents()))])?)
    }

    fn lines(&self, lines: &[OrderLine]) -> Result<Lines, ComposeError> {
        let sold: Vec<Line> = lines
            .iter()
            .map(|line| Line {
                unit_price: line.unit_price,
                qty: line.qty,
                vat_rate: line.vat_rate,
            })
            .collect();
        let mut rows = Vec::with_capacity(lines.len());
        for (line, domain_line) in lines.iter().zip(&sold) {
            let price = i18n::money(line.unit_price.cents());
            let amount = i18n::money(domain_line.total()?.cents());
            let id = if line.pay_what_you_want {
                "email-line-pay-what-you-want"
            } else {
                "email-line"
            };
            let text = self.t(
                id,
                &[
                    ("qty", line.qty.to_string()),
                    ("product", line.product.clone()),
                    ("amount", amount.clone()),
                    ("price", price.clone()),
                ],
            )?;
            let note = if line.pay_what_you_want {
                Some(self.t("email-line-your-price", &[])?)
            } else {
                None
            };
            rows.push(Row {
                product: line.product.clone(),
                note,
                qty: line.qty.to_string(),
                price,
                amount,
                text,
            });
        }
        let totals = VatTotals::from_lines(&sold)?;
        let mut vat = Vec::new();
        for rate in VatRate::ALL {
            let total = totals.rate(rate);
            if total.gross() == Money::ZERO {
                continue;
            }
            vat.push(Fact {
                label: self.t(
                    "email-vat",
                    &[
                        ("rate", rate.percent().to_string()),
                        ("net", i18n::money(total.net().cents())),
                    ],
                )?,
                value: i18n::money(total.vat().cents()),
            });
        }
        Ok(Lines {
            product: self.t("email-lines-product", &[])?,
            qty: self.t("email-lines-qty", &[])?,
            price: self.t("email-lines-price", &[])?,
            amount: self.t("email-lines-amount", &[])?,
            rows,
            total: Fact {
                label: self.t("email-total", &[])?,
                value: i18n::money(totals.gross()?.cents()),
            },
            vat,
        })
    }

    fn company_line(&self) -> Result<String, ComposeError> {
        let Company { name, address, kvk } = self.company;
        if [name, address, kvk]
            .iter()
            .any(|part| part.trim().is_empty())
        {
            return Err(ComposeError::Company);
        }
        let kvk = self.t("email-footer-kvk", &[("kvk", kvk.clone())])?;
        Ok([name.clone(), address.clone(), kvk].join(" · "))
    }
}

/// Whether `value` is exactly one email address, with nothing around it.
#[must_use]
pub fn is_address(value: &str) -> bool {
    lettre::Address::from_str(value).is_ok() && lettre::message::Mailbox::from_str(value).is_ok()
}

fn address(to: &str) -> Result<String, ComposeError> {
    let to = to.trim();
    lettre::Address::from_str(to).map_err(|_| ComposeError::Address)?;
    Ok(to.to_owned())
}

/// Links go into HTML attributes and the `List-Unsubscribe` header, so only plain https
/// URLs without spaces, quotes or angle brackets are accepted.
fn https(url: &str) -> Result<String, ComposeError> {
    let ok = url
        .strip_prefix("https://")
        .is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'))
        && url
            .chars()
            .all(|c| c.is_ascii_graphic() && !matches!(c, '"' | '\'' | '<' | '>' | '\\' | '`'));
    if ok {
        Ok(url.to_owned())
    } else {
        Err(ComposeError::Url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_must_be_plain_https() {
        assert!(https("https://broodvoorweldadigheid.nl/afmelden/abc?x=1&y=2").is_ok());
        for bad in [
            "http://broodvoorweldadigheid.nl/",
            "https://",
            "https:///x",
            "https://x/ y",
            "https://x/\"><script>",
            "https://x/\r\nBcc: a@b",
            "javascript:alert(1)",
            "https://x/é",
        ] {
            assert!(https(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn addresses_are_checked_and_trimmed() {
        assert_eq!(
            address(" klant@example.org ").expect("ok"),
            "klant@example.org"
        );
        assert!(address("klant").is_err());
        assert!(address("a@b\r\nBcc: c@d").is_err());
    }
}
