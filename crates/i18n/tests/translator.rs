#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::path::Path;

use i18n::{FluentArgs, I18nError, Translator};

fn nl() -> Translator {
    Translator::dutch().expect("Dutch bundle loads")
}

#[test]
fn simple_messages_translate() {
    let t = nl();
    assert_eq!(t.t("app-name", None).unwrap(), "Brood voor Weldadigheid");
    assert_eq!(t.t("saved", None).unwrap(), "Opgeslagen");
}

#[test]
fn arguments_have_no_unicode_isolation_marks() {
    let mut args = FluentArgs::new();
    args.set("name", "Anna");
    assert_eq!(
        nl().t("order-thanks", Some(&args)).unwrap(),
        "Bedankt voor je bestelling, Anna!"
    );
}

#[test]
fn preformatted_dutch_values_pass_through() {
    let mut args = FluentArgs::new();
    args.set("day", "za 6 dec");
    args.set("range", "11:00–12:00");
    assert_eq!(
        nl().t("pickup-moment", Some(&args)).unwrap(),
        "Ophaalmoment: za 6 dec, 11:00–12:00"
    );
}

#[test]
fn missing_message_is_a_typed_error() {
    assert_eq!(
        nl().t("does-not-exist", None),
        Err(I18nError::MissingMessage("does-not-exist".into()))
    );
}

#[test]
fn missing_argument_is_a_typed_error() {
    let err = nl().t("order-thanks", None).unwrap_err();
    assert!(
        matches!(&err, I18nError::Format { id, .. } if id == "order-thanks"),
        "{err:?}"
    );
}

#[test]
fn message_ids_lists_every_message_sorted() {
    let t = nl();
    assert_eq!(
        t.message_ids(),
        [
            "app-name",
            "date-long",
            "date-short",
            "error-back-home",
            "error-forbidden-text",
            "error-forbidden-title",
            "error-method-text",
            "error-method-title",
            "error-not-found-text",
            "error-not-found-title",
            "error-server-text",
            "error-server-title",
            "error-too-many-text",
            "error-too-many-title",
            "home-intro",
            "home-title",
            "month",
            "month-short",
            "order-deadline",
            "order-thanks",
            "pickup-moment",
            "saved",
            "skip-to-content",
            "tagline",
            "weekday",
            "weekday-short",
        ]
    );
}

#[test]
fn translator_can_be_shared_across_threads() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Translator>();
}

#[test]
fn every_dutch_ftl_file_is_compiled_in() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../locales/nl");
    let on_disk: BTreeSet<String> = std::fs::read_dir(&dir)
        .expect("locales/nl exists")
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|name| Path::new(name).extension().is_some_and(|ext| ext == "ftl"))
        .collect();
    let compiled: BTreeSet<String> = i18n::resource_files().map(str::to_owned).collect();
    assert_eq!(on_disk, compiled);
}
