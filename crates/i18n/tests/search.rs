#![forbid(unsafe_code)]

use i18n::search_key;

#[test]
fn case_and_accents_do_not_matter() {
    assert_eq!(search_key("Crème Brûlée"), "creme brulee");
    assert_eq!(search_key("CAFÉ De Hoek"), "cafe de hoek");
    assert_eq!(search_key("Ingrediënten"), "ingredienten");
    assert_eq!(search_key("Çà ñ ø å æ ß"), "ca n o a ae ss");
    assert_eq!(search_key("ĳsje Ĳssel"), "ijsje ijssel");
}

#[test]
fn other_text_stays() {
    assert_eq!(
        search_key("2026-0042 bas@example.nl"),
        "2026-0042 bas@example.nl"
    );
    assert_eq!(search_key("  Twee  woorden "), "  twee  woorden ");
    assert_eq!(search_key("€ 5,25 🍞"), "€ 5,25 🍞");
}
