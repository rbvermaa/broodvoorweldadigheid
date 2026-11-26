#![forbid(unsafe_code)]

use bakedays::PublicStatus;
use bakedays::calendar::{MenuItem, PublicCharity, PublicDrop, PublicPickup};
use bakedays::ical;
use domain::{BakeDayId, PickupSlotId, ProductId};
use i18n::Translator;
use jiff::Timestamp;

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn de_proef(slot: i64, starts: &str, ends: &str) -> PublicPickup {
    PublicPickup {
        slot_id: PickupSlotId::new(slot),
        location_name: "De Proef".to_owned(),
        address: "Majoor van Swietenlaan 15, 8382 CE Frederiksoord".to_owned(),
        starts_at: ts(starts),
        ends_at: ts(ends),
    }
}

fn drops() -> Vec<PublicDrop> {
    vec![
        PublicDrop {
            bake_day_id: BakeDayId::new(1),
            date: "2026-10-31".parse().unwrap(),
            status: PublicStatus::MenuAnnounced,
            cutoff_at: Some(ts("2026-10-29T19:00:00Z")),
            opens_at: None,
            pickups: vec![
                de_proef(11, "2026-10-31T10:00:00Z", "2026-10-31T11:00:00Z"),
                de_proef(12, "2026-10-31T11:00:00Z", "2026-10-31T11:30:00Z"),
            ],
            charity: Some(PublicCharity {
                name: "Stichting Voorbeeld; met, tekens\\".to_owned(),
                description: "Een lange beschrijving van een voorbeeldstichting die net iets te lang is voor één regel in een agenda."
                    .to_owned(),
                url: "https://example.org/doel".to_owned(),
            }),
            menu: Some(vec![
                MenuItem {
                    product_id: ProductId::new(1),
                    name: "Desem wit".to_owned(),
                    description: String::new(),
                },
                MenuItem {
                    product_id: ProductId::new(2),
                    name: "Spelt-rogge".to_owned(),
                    description: String::new(),
                },
            ]),
        },
        PublicDrop {
            bake_day_id: BakeDayId::new(2),
            date: "2026-11-07".parse().unwrap(),
            status: PublicStatus::Planned,
            cutoff_at: Some(ts("2026-11-05T19:00:00Z")),
            opens_at: None,
            pickups: vec![de_proef(21, "2026-11-07T10:00:00Z", "2026-11-07T11:00:00Z")],
            charity: None,
            menu: None,
        },
        PublicDrop {
            bake_day_id: BakeDayId::new(3),
            date: "2026-11-14".parse().unwrap(),
            status: PublicStatus::Cancelled,
            cutoff_at: Some(ts("2026-11-12T19:00:00Z")),
            opens_at: None,
            pickups: vec![de_proef(31, "2026-11-14T10:00:00Z", "2026-11-14T11:00:00Z")],
            charity: None,
            menu: None,
        },
    ]
}

fn render() -> String {
    let t = Translator::dutch().unwrap();
    ical::render(
        &drops(),
        &t,
        "broodvoorweldadigheid.nl",
        ts("2026-10-24T08:00:00Z"),
    )
    .unwrap()
}

#[test]
fn feed_matches_the_snapshot() {
    insta::assert_snapshot!(render().replace("\r\n", "\n"));
}

#[test]
fn every_line_ends_in_crlf_and_fits_75_octets() {
    let feed = render();
    assert!(feed.ends_with("END:VCALENDAR\r\n"));
    let body = feed.strip_suffix("\r\n").unwrap();
    for line in body.split("\r\n") {
        assert!(!line.contains('\n') && !line.contains('\r'), "{line:?}");
        assert!(line.len() <= 75, "{} octets: {line:?}", line.len());
    }
}

#[test]
fn one_event_per_pickup_slot_with_stable_uids() {
    let feed = render();
    let uids: Vec<&str> = feed
        .split("\r\n")
        .filter_map(|l| l.strip_prefix("UID:"))
        .collect();
    assert_eq!(
        uids,
        [
            "pickup-slot-11@broodvoorweldadigheid.nl",
            "pickup-slot-12@broodvoorweldadigheid.nl",
            "pickup-slot-21@broodvoorweldadigheid.nl",
            "pickup-slot-31@broodvoorweldadigheid.nl",
        ]
    );
    assert_eq!(feed, render());
}

#[test]
fn empty_calendar_has_no_events() {
    let t = Translator::dutch().unwrap();
    let feed = ical::render(
        &[],
        &t,
        "broodvoorweldadigheid.nl",
        ts("2026-10-24T08:00:00Z"),
    )
    .unwrap();
    assert!(feed.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n"));
    assert!(!feed.contains("BEGIN:VEVENT"));
}
