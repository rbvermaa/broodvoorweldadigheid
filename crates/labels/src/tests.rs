use std::collections::BTreeSet;
use std::sync::LazyLock;

use domain::Allergen;
use i18n::Translator;
use jiff::civil::{Date, date};

use crate::*;

static T: LazyLock<Translator> = LazyLock::new(|| Translator::dutch().expect("Dutch bundle"));

fn ingredient(name: &str, bold: bool) -> Ingredient {
    Ingredient {
        name: name.to_owned(),
        bold,
        parts: Vec::new(),
    }
}

fn compound(name: &str, parts: Vec<Ingredient>) -> Ingredient {
    Ingredient {
        name: name.to_owned(),
        bold: false,
        parts,
    }
}

fn business() -> Business {
    Business {
        name: "Brood voor Weldadigheid".to_owned(),
        address: "Voorbeeldlaan 1, 1234 AB Voorbeeld".to_owned(),
    }
}

fn granola() -> Label {
    Label {
        kind: Kind::Product {
            storage: "Droog en koel bewaren.".to_owned(),
        },
        product: "Granola met noten".to_owned(),
        ingredients: vec![
            ingredient("havervlokken", true),
            ingredient("walnoten", true),
            ingredient("honing", false),
            ingredient("zonnebloemolie", false),
        ],
        may_contain: BTreeSet::from([Allergen::Milk, Allergen::SesameSeeds]),
        net_weight_g: 350,
        best_before: date(2027, 2, 3),
        batch_code: "261205-2".to_owned(),
        business: business(),
    }
}

fn frozen_bread() -> Label {
    Label {
        kind: Kind::Freezer {
            frozen_on: date(2026, 12, 5),
        },
        product: "Desembrood wit".to_owned(),
        ingredients: vec![
            ingredient("tarwebloem", true),
            ingredient("water", false),
            ingredient("zout", false),
        ],
        may_contain: BTreeSet::new(),
        net_weight_g: 800,
        best_before: date(2027, 3, 5),
        batch_code: "261205-1".to_owned(),
        business: business(),
    }
}

fn long_list(n: usize) -> Label {
    Label {
        ingredients: (0..n)
            .map(|i| ingredient(&format!("ingrediënt nummer {i}"), i % 3 == 0))
            .collect(),
        ..granola()
    }
}

fn png_size(bytes: &[u8]) -> (u32, u32) {
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    let word = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    (word(16), word(20))
}

fn sorted_json(mut value: serde_json::Value) -> String {
    value.sort_all_objects();
    serde_json::to_string_pretty(&value).unwrap()
}

#[test]
fn sizes_are_whole_dots_at_300_dpi() {
    let size = |length_mm| Size {
        width_mm: WIDTH_MM,
        length_mm,
    };
    assert_eq!(size(30).pixels(), (591, 354));
    assert_eq!(size(40).pixels(), (591, 472));
    assert_eq!(size(50).pixels(), (591, 591));
    assert_eq!(size(70).pixels(), (591, 827));
    assert_eq!(size(100).pixels(), (591, 1181));
}

#[test]
fn a_short_label_is_one_png_of_50_by_30_mm() {
    for label in [granola(), frozen_bread()] {
        let pngs = render(&label, &T).unwrap();
        assert_eq!(pngs.len(), 1);
        assert_eq!(pngs[0].side, Side::Whole);
        assert_eq!(
            pngs[0].size,
            Size {
                width_mm: 50,
                length_mm: 30
            }
        );
        assert_eq!(png_size(&pngs[0].bytes), (591, 354));
    }
}

#[test]
fn pngs_are_the_same_for_the_same_label_and_only_black_and_white() {
    let first = render(&frozen_bread(), &T).unwrap();
    assert_eq!(first, render(&frozen_bread(), &T).unwrap());
    let decoded = resvg::tiny_skia::Pixmap::decode_png(&first[0].bytes).unwrap();
    let mut black = 0;
    for pixel in decoded.pixels() {
        let (r, g, b, a) = (pixel.red(), pixel.green(), pixel.blue(), pixel.alpha());
        assert_eq!(a, 255);
        assert!(r == g && g == b && (r == 0 || r == 255), "grey pixel {r}");
        black += usize::from(r == 0);
    }
    assert!(black > 1000, "the label has ink: {black} black pixels");
}

#[test]
fn a_longer_list_takes_a_longer_label() {
    let pngs = render(&long_list(12), &T).unwrap();
    assert_eq!(pngs.len(), 1);
    assert!(pngs[0].size.length_mm > 30, "{:?}", pngs[0].size);
    assert_eq!(png_size(&pngs[0].bytes), pngs[0].size.pixels());
}

#[test]
fn a_list_too_long_for_one_label_is_split_over_front_and_back() {
    let pngs = render(&long_list(70), &T).unwrap();
    let sides: Vec<Side> = pngs.iter().map(|p| p.side).collect();
    assert_eq!(sides, [Side::Front, Side::Back]);
    for png in &pngs {
        assert_eq!(png_size(&png.bytes), png.size.pixels());
    }
    let text = text(&long_list(70), &T).unwrap();
    let back = text.split("--- back").nth(1).unwrap();
    assert!(back.contains("Ingrediënten:"), "{back}");
    let front = text.split("--- back").next().unwrap();
    assert!(!front.contains("Ingrediënten:"), "{front}");
    assert!(front.contains("261205-2"), "{front}");
}

#[test]
fn a_list_that_fits_nowhere_is_refused() {
    assert!(matches!(render(&long_list(400), &T), Err(Error::TooLong)));
}

#[test]
fn product_label_text() {
    insta::assert_snapshot!(text(&granola(), &T).unwrap());
}

#[test]
fn freezer_label_text() {
    insta::assert_snapshot!(text(&frozen_bread(), &T).unwrap());
}

#[test]
fn product_label_data() {
    insta::assert_snapshot!(sorted_json(data(&granola(), Side::Whole, &T).unwrap()));
}

#[test]
fn freezer_label_data() {
    insta::assert_snapshot!(sorted_json(data(&frozen_bread(), Side::Whole, &T).unwrap()));
}

#[test]
fn csv_rows_for_the_niimbot_import() {
    let granola = granola();
    let bread = frozen_bread();
    let csv = csv(
        &[
            CsvRow {
                label: &granola,
                copies: 24,
                file: "261205-2-product.png",
            },
            CsvRow {
                label: &bread,
                copies: 6,
                file: "261205-1-vriezer.png",
            },
        ],
        &T,
    )
    .unwrap();
    assert!(csv.starts_with('\u{feff}'));
    assert!(csv.ends_with("\r\n"));
    insta::assert_snapshot!(csv.trim_start_matches('\u{feff}').replace("\r\n", "\n"));
}

#[test]
fn best_before_counts_calendar_days() {
    assert_eq!(
        best_before(date(2026, 12, 5), 90).unwrap(),
        date(2027, 3, 5)
    );
    assert_eq!(
        best_before(date(2027, 3, 1), 30).unwrap(),
        date(2027, 3, 31)
    );
    assert_eq!(
        best_before(date(2028, 2, 28), 1).unwrap(),
        date(2028, 2, 29)
    );
    assert!(matches!(
        best_before(Date::MAX, 1),
        Err(Error::DateOutOfRange)
    ));
}

#[test]
fn a_glyph_no_font_has_is_an_error() {
    let label = Label {
        product: "Brood \u{1F35E}".to_owned(),
        ..granola()
    };
    assert!(matches!(render(&label, &T), Err(Error::Typst(_))));
}

#[test]
fn a_word_too_long_for_the_width_is_refused_not_cut_off() {
    let long_name = Label {
        product: "Volkorenspeltdesembroodmetzonnebloempitten".to_owned(),
        ..granola()
    };
    assert!(matches!(render(&long_name, &T), Err(Error::TooWide)));
    let long_ingredient = Label {
        ingredients: vec![ingredient(&"tarwe".repeat(20), true)],
        ..granola()
    };
    assert!(matches!(render(&long_ingredient, &T), Err(Error::TooWide)));
}

#[test]
fn drawing_errors_keep_their_message() {
    assert!(matches!(png::draw_error("bad svg"), Error::Draw(m) if m == "bad svg"));
}

fn cinnamon_bun() -> Label {
    Label {
        kind: Kind::Product {
            storage: "Bewaren bij kamertemperatuur.".to_owned(),
        },
        product: "Kaneelbol".to_owned(),
        ingredients: vec![
            ingredient("tarwebloem", true),
            ingredient("water", false),
            compound(
                "vulling",
                vec![
                    ingredient("suiker", false),
                    ingredient("boter", true),
                    ingredient("kaneel", false),
                ],
            ),
            ingredient("zout", false),
        ],
        may_contain: BTreeSet::from([Allergen::Nuts]),
        net_weight_g: 110,
        best_before: date(2026, 12, 7),
        batch_code: "261205-3".to_owned(),
        business: business(),
    }
}

#[test]
fn a_compound_ingredient_label_text() {
    insta::assert_snapshot!(text(&cinnamon_bun(), &T).unwrap());
}

#[test]
fn a_compound_ingredient_in_the_csv_keeps_capitals_inside_the_brackets() {
    let bun = cinnamon_bun();
    let csv = csv(
        &[CsvRow {
            label: &bun,
            copies: 12,
            file: "261205-3-product.png",
        }],
        &T,
    )
    .unwrap();
    assert!(
        csv.contains(";TARWEBLOEM, water, vulling (suiker, BOTER, kaneel), zout;"),
        "{csv}"
    );
    let data = data(&bun, Side::Whole, &T).unwrap();
    assert_eq!(data["ingredients"][2]["name"], "vulling");
    assert_eq!(data["ingredients"][2]["bold"], false);
    assert_eq!(data["ingredients"][2]["parts"][1]["bold"], true);
}

/// Draws every sample label; with `LABEL_SAMPLES` set to a directory, also writes the
/// PNGs there for a look by eye.
#[test]
fn sample_labels_render() {
    let samples = [
        ("granola", granola()),
        ("vriezer-desembrood", frozen_bread()),
        ("kaneelbol-vulling", cinnamon_bun()),
    ];
    let out = std::env::var_os("LABEL_SAMPLES").map(std::path::PathBuf::from);
    for (name, label) in samples {
        for png in render(&label, &T).unwrap() {
            assert_eq!(png_size(&png.bytes), png.size.pixels());
            if let Some(dir) = &out {
                let file = dir.join(format!("{name}-{}.png", png.side.part()));
                std::fs::write(file, &png.bytes).unwrap();
            }
        }
    }
}
