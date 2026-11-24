//! What an email says, already translated, and its two renderings: plain text and HTML.

use std::fmt::Write as _;

use askama::Template;

use crate::style::{self, StyleError};

pub(crate) struct Content {
    pub subject: String,
    pub brand: String,
    pub tagline: String,
    pub heading: String,
    pub greeting: String,
    pub blocks: Vec<Block>,
    pub signoff: String,
    /// Why the reader gets this email; drop alerts only.
    pub reason: Option<String>,
    pub unsubscribe: Option<Link>,
    pub company: String,
}

pub(crate) enum Block {
    Paragraph(String),
    Quote(Vec<String>),
    Facts(Vec<Fact>),
    Lines(Lines),
    Items(Vec<String>),
    Button(Link),
}

pub(crate) struct Fact {
    pub label: String,
    pub value: String,
}

pub(crate) struct Link {
    pub label: String,
    pub url: String,
}

pub(crate) struct Lines {
    pub product: String,
    pub qty: String,
    pub price: String,
    pub amount: String,
    pub rows: Vec<Row>,
    pub total: Fact,
    pub vat: Vec<Fact>,
}

pub(crate) struct Row {
    pub product: String,
    pub note: Option<String>,
    pub qty: String,
    pub price: String,
    pub amount: String,
    /// The whole line as one sentence, for the plain-text part.
    pub text: String,
}

#[derive(Template)]
#[template(path = "email/layout.html")]
struct Html<'a> {
    content: &'a Content,
}

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("cannot render the email template: {0}")]
    Template(#[from] askama::Error),
    #[error(transparent)]
    Style(#[from] StyleError),
}

impl Content {
    pub fn html(&self) -> Result<String, RenderError> {
        let html = Html { content: self }.render()?;
        Ok(style::inline(&html)?)
    }

    pub fn text(&self) -> String {
        let mut out = String::new();
        let mut paragraph = |text: &str| {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
            out.push('\n');
        };
        paragraph(&self.heading);
        paragraph(&self.greeting);
        for block in &self.blocks {
            paragraph(&block.text());
        }
        paragraph(&self.signoff);
        let mut footer = String::from("-- \n");
        if let Some(reason) = &self.reason {
            footer.push_str(reason);
            footer.push('\n');
        }
        if let Some(link) = &self.unsubscribe {
            let _ = writeln!(footer, "{}: {}", link.label, link.url);
        }
        footer.push_str(&self.company);
        paragraph(&footer);
        out
    }
}

impl Block {
    fn text(&self) -> String {
        let mut out = String::new();
        match self {
            Self::Paragraph(text) => out.push_str(text),
            Self::Quote(lines) => {
                for line in lines {
                    let _ = writeln!(out, "> {line}");
                }
            }
            Self::Facts(facts) => {
                for fact in facts {
                    let _ = writeln!(out, "{}: {}", fact.label, fact.value);
                }
            }
            Self::Lines(lines) => {
                for row in &lines.rows {
                    let _ = writeln!(out, "{}", row.text);
                }
                out.push('\n');
                let _ = writeln!(out, "{}: {}", lines.total.label, lines.total.value);
                for vat in &lines.vat {
                    let _ = writeln!(out, "{}: {}", vat.label, vat.value);
                }
            }
            Self::Items(items) => {
                for item in items {
                    let _ = writeln!(out, "- {item}");
                }
            }
            Self::Button(link) => {
                let _ = write!(out, "{}: {}", link.label, link.url);
            }
        }
        out.trim_end().to_owned()
    }
}
