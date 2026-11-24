pub const TOKENS_CSS: &str = include_str!(concat!(env!("OUT_DIR"), "/tokens.css"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_css_is_generated_from_design_tokens() {
        let expected = crate::tokens::generate(include_str!("../../../design/tokens.toml"))
            .expect("design/tokens.toml is valid");
        assert_eq!(TOKENS_CSS, expected);
    }

    #[test]
    fn tokens_css_has_brand_base_colours_and_fonts() {
        assert!(TOKENS_CSS.contains("  --color-black: #000000;\n"));
        assert!(TOKENS_CSS.contains("  --color-white: #ffffff;\n"));
        assert!(TOKENS_CSS.contains("  --font-heading: \"Oswald\""));
        assert!(TOKENS_CSS.contains("  --font-body: \"Inter\""));
    }
}
