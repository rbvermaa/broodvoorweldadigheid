#![forbid(unsafe_code)]

use catalog::markdown::{Block, Span, parse};

fn text(text: &str) -> Span {
    Span {
        text: text.to_owned(),
        bold: false,
        italic: false,
        link: None,
    }
}

fn bold(text: &str) -> Span {
    Span {
        bold: true,
        ..self::text(text)
    }
}

fn italic(text: &str) -> Span {
    Span {
        italic: true,
        ..self::text(text)
    }
}

fn link(text: &str, url: &str) -> Span {
    Span {
        link: Some(url.to_owned()),
        ..self::text(text)
    }
}

#[test]
fn blank_lines_split_paragraphs_and_single_newlines_join_lines() {
    assert_eq!(
        parse("Eerste regel\ntweede regel\r\n\r\n\n  Nieuwe alinea  "),
        vec![
            Block::Paragraph(vec![text("Eerste regel tweede regel")]),
            Block::Paragraph(vec![text("Nieuwe alinea")]),
        ]
    );
    assert_eq!(parse(""), vec![]);
    assert_eq!(parse(" \n\n "), vec![]);
}

#[test]
fn bold_and_italic() {
    assert_eq!(
        parse("Bestel **voor donderdag** en *haal* op"),
        vec![Block::Paragraph(vec![
            text("Bestel "),
            bold("voor donderdag"),
            text(" en "),
            italic("haal"),
            text(" op"),
        ])]
    );
    assert_eq!(
        parse("***beide***"),
        vec![Block::Paragraph(vec![Span {
            bold: true,
            italic: true,
            ..text("beide")
        }])]
    );
}

#[test]
fn unclosed_markers_stay_as_text() {
    assert_eq!(
        parse("2 * 3 en **niet dicht"),
        vec![Block::Paragraph(vec![text("2 * 3 en **niet dicht")])]
    );
}

#[test]
fn backslash_escapes_a_marker() {
    assert_eq!(
        parse(r"\*geen\* cursief, \[geen link\]"),
        vec![Block::Paragraph(vec![text("*geen* cursief, [geen link]")])]
    );
}

#[test]
fn https_links_become_links() {
    assert_eq!(
        parse("Kijk op [de site](https://broodvoorweldadigheid.nl/drops?x=1&y=2)."),
        vec![Block::Paragraph(vec![
            text("Kijk op "),
            link("de site", "https://broodvoorweldadigheid.nl/drops?x=1&y=2"),
            text("."),
        ])]
    );
    assert_eq!(
        parse("**[vet](https://example.org)**"),
        vec![Block::Paragraph(vec![Span {
            bold: true,
            ..link("vet", "https://example.org")
        }])]
    );
}

#[test]
fn other_links_keep_only_their_text() {
    for url in [
        "javascript:alert(1)",
        "JavaScript:alert(1)",
        "http://example.org",
        "data:text/html,hoi",
        "//example.org",
        "/relatief",
        "https://",
        "https://x/\"onmouseover=\"alert(1)",
        "https://x/ y",
        "mailto:bakker@example.org",
    ] {
        let blocks = parse(&format!("[klik]({url}) daarna"));
        let Block::Paragraph(spans) = &blocks[0] else {
            panic!("{url}: {blocks:?}");
        };
        assert!(
            spans.iter().all(|span| span.link.is_none()),
            "{url}: {spans:?}"
        );
        let all: String = spans.iter().map(|span| span.text.as_str()).collect();
        assert!(all.starts_with("klik"), "{url}: {all}");
        assert!(!all.contains("javascript"), "{url}: {all}");
    }
}

#[test]
fn html_is_kept_as_plain_text_for_the_template_to_escape() {
    assert_eq!(
        parse("<script>alert(1)</script> & <b>vet</b>"),
        vec![Block::Paragraph(vec![text(
            "<script>alert(1)</script> & <b>vet</b>"
        )])]
    );
}

#[test]
fn dash_or_star_lines_make_a_list() {
    assert_eq!(
        parse("Neem mee:\n\n- een **tas**\n* je bestelnummer\n  op je telefoon\n\nTot zaterdag!"),
        vec![
            Block::Paragraph(vec![text("Neem mee:")]),
            Block::List(vec![
                vec![text("een "), bold("tas")],
                vec![text("je bestelnummer op je telefoon")],
            ]),
            Block::Paragraph(vec![text("Tot zaterdag!")]),
        ]
    );
}

#[test]
fn a_list_right_after_a_paragraph_line_starts_a_list() {
    assert_eq!(
        parse("Neem mee:\n- een tas"),
        vec![
            Block::Paragraph(vec![text("Neem mee:")]),
            Block::List(vec![vec![text("een tas")]]),
        ]
    );
}
