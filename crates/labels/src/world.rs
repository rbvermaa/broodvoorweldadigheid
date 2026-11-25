//! Typst as a library, as in `production` (ADR 0016): the template can read only itself,
//! `/data.json` and the logo, and only the pinned fonts exist, so a label depends on
//! nothing but its input.

use std::sync::LazyLock;

use serde_json::Value;
use typst::diag::{FileError, FileResult, SourceDiagnostic};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_layout::PagedDocument;

use crate::Error;

const TEMPLATE: &str = include_str!("../../../typst/label.typ");
const LOGO: &[u8] = include_bytes!("../../../design/logo.png");

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

struct LabelWorld {
    main: Source,
    data: Bytes,
}

impl World for LabelWorld {
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
        if id == file_id("/data.json") {
            Ok(self.data.clone())
        } else if id == file_id("/logo.png") {
            Ok(Bytes::new(LOGO))
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

/// Lays out the label template with `data` as `/data.json`. A warning, such as a glyph
/// none of the fonts has, is an error: a label must print exactly its text.
pub(crate) fn layout(data: &Value) -> Result<PagedDocument, Error> {
    let world = LabelWorld {
        main: Source::new(file_id("/label.typ"), TEMPLATE.to_owned()),
        data: Bytes::from_string(data.to_string()),
    };
    let compiled = typst::compile::<PagedDocument>(&world);
    // Typst memoizes globally; without eviction every label stays in memory.
    typst::comemo::evict(10);
    let document = compiled
        .output
        .map_err(|errors| Error::Typst(messages(&errors)))?;
    if !compiled.warnings.is_empty() {
        return Err(Error::Typst(messages(&compiled.warnings)));
    }
    if document
        .pages()
        .iter()
        .any(|page| missing_glyph(&page.frame))
    {
        return Err(Error::Typst("a character none of the fonts has".to_owned()));
    }
    Ok(document)
}

/// Glyph 0 is a font's "missing" box; Typst draws it without a warning.
fn missing_glyph(frame: &typst::layout::Frame) -> bool {
    use typst::layout::FrameItem;
    frame.items().any(|(_, item)| match item {
        FrameItem::Group(group) => missing_glyph(&group.frame),
        FrameItem::Text(text) => text.glyphs.iter().any(|g| g.id == 0),
        _ => false,
    })
}
