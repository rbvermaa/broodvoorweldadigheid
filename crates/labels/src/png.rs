//! Typst layout to a 300-dpi PNG at the label's exact size.

use i18n::Translator;
use resvg::tiny_skia::{Color, Pixmap, Transform};
use resvg::usvg;
use typst_layout::PagedDocument;

use crate::{DPI, Error, LENGTHS_MM, Label, Size, WIDTH_MM, data, world};

/// Which part of a label a PNG holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// All of the label on one.
    Whole,
    /// Name, dates, batch code, storage and business; when the whole does not fit.
    Front,
    /// Ingredients and may-contain.
    Back,
}

impl Side {
    pub(crate) const fn part(self) -> &'static str {
        match self {
            Self::Whole => "whole",
            Self::Front => "front",
            Self::Back => "back",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Png {
    pub side: Side,
    pub size: Size,
    pub bytes: Vec<u8>,
}

const MM_PER_PT: f64 = 25.4 / 72.0;

struct Laid {
    side: Side,
    document: PagedDocument,
    size: Size,
}

/// Lays out one side; `None` when it fits none of the label lengths.
fn lay(label: &Label, side: Side, t: &Translator) -> Result<Option<Laid>, Error> {
    let document = world::layout(&data(label, side, t)?)?;
    let [page] = document.pages() else {
        return Ok(None);
    };
    let needed_mm = page.frame.height().to_pt() * MM_PER_PT;
    Ok(LENGTHS_MM
        .into_iter()
        .find(|&length| f64::from(length) >= needed_mm)
        .map(|length_mm| Laid {
            side,
            document,
            size: Size {
                width_mm: WIDTH_MM,
                length_mm,
            },
        }))
}

/// The label on the shortest length it fits, or split over a front and a back label.
fn sides(label: &Label, t: &Translator) -> Result<Vec<Laid>, Error> {
    if let Some(whole) = lay(label, Side::Whole, t)? {
        return Ok(vec![whole]);
    }
    match (lay(label, Side::Front, t)?, lay(label, Side::Back, t)?) {
        (Some(front), Some(back)) => Ok(vec![front, back]),
        _ => Err(Error::TooLong),
    }
}

/// Draws the laid-out page black on white: the B21 Pro prints dots, not greys, so every
/// pixel is either. The page sits at the top of a label that may be longer.
fn draw(laid: &Laid) -> Result<Png, Error> {
    let page = &laid.document.pages()[0];
    let svg = typst_svg::svg(page, &typst_svg::SvgOptions::default());
    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default())
        .map_err(|e| Error::Draw(e.to_string()))?;
    let (width, height) = laid.size.pixels();
    let mut pixmap = Pixmap::new(width, height)
        .ok_or_else(|| Error::Draw(format!("no {width} x {height} canvas")))?;
    pixmap.fill(Color::WHITE);
    // typst-svg sizes the page in pt, which usvg reads as CSS units at 96 per inch.
    #[expect(clippy::cast_precision_loss, reason = "300 / 96 is exact in f32")]
    let scale = DPI as f32 / 96.0;
    resvg::render(
        &tree,
        Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    for pixel in pixmap.data_mut().as_chunks_mut::<4>().0 {
        let ink = if pixel[0] < 128 { 0 } else { 255 };
        pixel[..3].fill(ink);
        pixel[3] = 255;
    }
    let bytes = pixmap
        .encode_png()
        .map_err(|e| Error::Draw(e.to_string()))?;
    Ok(Png {
        side: laid.side,
        size: laid.size,
        bytes,
    })
}

/// The PNGs of one label design: one, or a front and a back.
///
/// # Errors
/// [`Error::TooLong`] when even a front and back do not fit; [`Error::Typst`] or
/// [`Error::Draw`] when the template fails.
pub fn render(label: &Label, t: &Translator) -> Result<Vec<Png>, Error> {
    sides(label, t)?.iter().map(draw).collect()
}

/// The text of each label side, line by line, with bold runs between `*`. Tests snapshot
/// this instead of the pixels.
///
/// # Errors
/// As [`render`].
pub fn text(label: &Label, t: &Translator) -> Result<String, Error> {
    use typst::layout::{Frame, FrameItem, Point};

    fn collect(frame: &Frame, origin: Point, out: &mut Vec<(i64, i64, String)>) {
        for (pos, item) in frame.items() {
            let at = origin + *pos;
            match item {
                FrameItem::Group(group) => collect(&group.frame, at, out),
                FrameItem::Text(text) => {
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "hundredths of a point on a label fit easily"
                    )]
                    let round = |v: f64| (v * 100.0).round() as i64;
                    let run = text.text.to_string();
                    let bold = text.font.info().variant.weight.to_number() >= 600;
                    out.push((
                        round(at.y.to_pt()),
                        round(at.x.to_pt()),
                        if bold { format!("*{run}*") } else { run },
                    ));
                }
                _ => {}
            }
        }
    }

    let mut out = Vec::new();
    for laid in sides(label, t)? {
        let mut runs = Vec::new();
        collect(&laid.document.pages()[0].frame, Point::zero(), &mut runs);
        runs.sort();
        let mut lines: Vec<(i64, Vec<String>)> = Vec::new();
        for (y, _, run) in runs {
            match lines.last_mut() {
                Some((line_y, line)) if *line_y == y => line.push(run),
                _ => lines.push((y, vec![run])),
            }
        }
        out.push(format!(
            "--- {} {} x {} mm ---\n{}",
            laid.side.part(),
            laid.size.width_mm,
            laid.size.length_mm,
            lines
                .into_iter()
                .map(|(_, line)| line.join(" | "))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    Ok(out.join("\n"))
}
