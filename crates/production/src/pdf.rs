//! Typst as a library. The only files a template can read are itself and `/data.json`;
//! the only fonts are the pinned ones compiled in (ADR 0009), so a PDF depends on
//! nothing but its input.

use std::sync::LazyLock;

use serde_json::Value;
use typst::diag::{FileError, FileResult, SourceDiagnostic};
use typst::foundations::{Bytes, Datetime, Duration, Smart};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_layout::PagedDocument;
use typst_pdf::PdfOptions;

use crate::Error;

pub(crate) struct Template {
    path: &'static str,
    source: &'static str,
}

#[cfg(test)]
impl Template {
    pub(crate) const fn inline(path: &'static str, source: &'static str) -> Self {
        Self { path, source }
    }
}

pub(crate) const PRODUCTION: Template = Template {
    path: "/production.typ",
    source: include_str!("../../../typst/production.typ"),
};

pub(crate) const PICKUP: Template = Template {
    path: "/pickup.typ",
    source: include_str!("../../../typst/pickup.typ"),
};

pub(crate) const NEEDS: Template = Template {
    path: "/ingredient_needs.typ",
    source: include_str!("../../../typst/ingredient_needs.typ"),
};

macro_rules! font_file {
    ($name:literal) => {
        include_bytes!(concat!(
            env!(
                "FONTS_DIR",
                "FONTS_DIR is not set; build inside `nix develop` or with `nix build`"
            ),
            "/",
            $name
        ))
    };
}

const FONT_FILES: [&[u8]; 3] = [
    font_file!("inter-regular.ttf"),
    font_file!("inter-bold.ttf"),
    font_file!("oswald.ttf"),
];

struct Fonts {
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
}

static FONTS: LazyLock<Fonts> = LazyLock::new(|| {
    let fonts: Vec<Font> = FONT_FILES
        .iter()
        .flat_map(|data| Font::iter(Bytes::new(*data)))
        .collect();
    Fonts {
        book: LazyHash::new(FontBook::from_fonts(&fonts)),
        fonts,
    }
});

static LIBRARY: LazyLock<LazyHash<Library>> = LazyLock::new(|| LazyHash::new(Library::default()));

fn file_id(path: &str) -> FileId {
    let vpath = VirtualPath::new(path).expect("a fixed absolute path");
    RootedPath::new(VirtualRoot::Project, vpath).intern()
}

struct Paper {
    main: Source,
    data_id: FileId,
    data: Bytes,
}

impl World for Paper {
    fn library(&self) -> &LazyHash<Library> {
        &LIBRARY
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &FONTS.book
    }

    fn main(&self) -> FileId {
        self.main.id()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main.id() {
            Ok(self.main.clone())
        } else {
            Err(FileError::AccessDenied)
        }
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        if id == self.data_id {
            Ok(self.data.clone())
        } else {
            Err(FileError::AccessDenied)
        }
    }

    fn font(&self, index: usize) -> Option<Font> {
        FONTS.fonts.get(index).cloned()
    }

    fn today(&self, _offset: Option<Duration>) -> Option<Datetime> {
        None
    }
}

fn messages(diagnostics: &[SourceDiagnostic]) -> String {
    diagnostics
        .iter()
        .map(|d| d.message.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Lays out a template with `data` as `/data.json`. Warnings, such as a glyph none of the
/// fonts has, are returned next to the document.
pub(crate) fn layout(
    template: &Template,
    data: &Value,
) -> Result<(PagedDocument, Vec<String>), Error> {
    let world = Paper {
        main: Source::new(file_id(template.path), template.source.to_owned()),
        data_id: file_id("/data.json"),
        data: Bytes::from_string(data.to_string()),
    };
    let compiled = typst::compile::<PagedDocument>(&world);
    // Typst memoizes globally; without eviction every rendered list stays in memory.
    typst::comemo::evict(10);
    let warnings = compiled
        .warnings
        .iter()
        .map(|w| w.message.to_string())
        .collect();
    let document = compiled
        .output
        .map_err(|errors| Error::Pdf(messages(&errors)))?;
    Ok((document, warnings))
}

/// The PDF of a template. `ident` names the document; it and the missing creation date
/// keep the bytes the same for the same input.
pub(crate) fn render(template: &Template, data: &Value, ident: String) -> Result<Vec<u8>, Error> {
    let (document, _warnings) = layout(template, data)?;
    let options = PdfOptions {
        ident: Smart::Custom(ident),
        creator: Smart::Custom(None),
        timestamp: None,
        ..PdfOptions::default()
    };
    typst_pdf::pdf(&document, &options).map_err(|errors| Error::Pdf(messages(&errors)))
}

/// The text of a laid-out document, line by line from the top of each page; runs on one
/// line are joined by ` | `. Tests snapshot this instead of the PDF bytes.
#[cfg(test)]
pub(crate) fn text(document: &PagedDocument) -> String {
    use typst::layout::{Frame, FrameItem, Point};

    fn collect(frame: &Frame, origin: Point, out: &mut Vec<(i64, i64, String)>) {
        for (pos, item) in frame.items() {
            let at = origin + *pos;
            match item {
                FrameItem::Group(group) => collect(&group.frame, at, out),
                FrameItem::Text(text) => {
                    // Positions in hundredths of a point keep the sort stable.
                    let round = |v: f64| (v * 100.0).round() as i64;
                    out.push((
                        round(at.y.to_pt()),
                        round(at.x.to_pt()),
                        text.text.to_string(),
                    ));
                }
                _ => {}
            }
        }
    }

    let mut pages = Vec::new();
    for page in document.pages() {
        let mut runs = Vec::new();
        collect(&page.frame, Point::zero(), &mut runs);
        runs.sort();
        let mut lines: Vec<(i64, Vec<String>)> = Vec::new();
        for (y, _, run) in runs {
            match lines.last_mut() {
                Some((line_y, line)) if *line_y == y => line.push(run),
                _ => lines.push((y, vec![run])),
            }
        }
        pages.push(
            lines
                .into_iter()
                .map(|(_, line)| line.join(" | "))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    pages.join("\n--- page ---\n")
}
