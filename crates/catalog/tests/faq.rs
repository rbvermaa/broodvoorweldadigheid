#![forbid(unsafe_code)]

use catalog::faq::{self, FaqId, FaqInput};
use catalog::{Error, Faq};
use jiff::{SignedDuration, Timestamp};
use sqlx::pool::PoolConnection;
use sqlx::{Executor, Postgres};
use testkit::FakeClock;

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn input(question: &str) -> FaqInput {
    FaqInput {
        question: question.to_owned(),
        answer_md: format!("Antwoord op *{question}*"),
    }
}

async fn empty(conn: &mut PoolConnection<Postgres>) {
    conn.execute("DELETE FROM faq_entry").await.unwrap();
}

fn questions(entries: &[Faq]) -> Vec<&str> {
    entries
        .iter()
        .map(|entry| entry.question.as_str())
        .collect()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn four_published_questions_are_seeded(mut conn: PoolConnection<Postgres>) {
    let published = faq::list_published(&mut conn).await.unwrap();
    assert_eq!(published.len(), 4);
    assert_eq!(
        published
            .iter()
            .map(|entry| entry.position)
            .collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    assert!(published.iter().all(|entry| entry.published));
    assert!(
        published
            .iter()
            .any(|entry| entry.answer_md.contains("De Proef"))
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn new_entries_go_last_unpublished(mut conn: PoolConnection<Postgres>) {
    empty(&mut conn).await;
    let clock = FakeClock::new(ts("2026-12-01T09:00:00Z"));
    let first = faq::create(&mut conn, &clock, &input("Eerste"))
        .await
        .unwrap();
    let second = faq::create(&mut conn, &clock, &input(" Tweede "))
        .await
        .unwrap();

    assert_eq!((first.position, second.position), (0, 1));
    assert_eq!(second.question, "Tweede");
    assert!(!first.published);
    assert_eq!(first.created_at, ts("2026-12-01T09:00:00Z"));
    assert_eq!(first.updated_at, first.created_at);
    assert_eq!(
        questions(&faq::list_all(&mut conn).await.unwrap()),
        ["Eerste", "Tweede"]
    );
    assert!(faq::list_published(&mut conn).await.unwrap().is_empty());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn only_published_entries_are_listed_publicly_in_order(mut conn: PoolConnection<Postgres>) {
    empty(&mut conn).await;
    let clock = FakeClock::new(ts("2026-12-01T09:00:00Z"));
    let a = faq::create(&mut conn, &clock, &input("A")).await.unwrap();
    let b = faq::create(&mut conn, &clock, &input("B")).await.unwrap();
    let c = faq::create(&mut conn, &clock, &input("C")).await.unwrap();

    clock.advance(SignedDuration::from_mins(5));
    let published = faq::set_published(&mut conn, &clock, c.id, true)
        .await
        .unwrap();
    assert!(published.published);
    assert_eq!(published.updated_at, ts("2026-12-01T09:05:00Z"));
    faq::set_published(&mut conn, &clock, a.id, true)
        .await
        .unwrap();
    assert_eq!(
        questions(&faq::list_published(&mut conn).await.unwrap()),
        ["A", "C"]
    );

    faq::set_published(&mut conn, &clock, a.id, false)
        .await
        .unwrap();
    assert_eq!(
        questions(&faq::list_published(&mut conn).await.unwrap()),
        ["C"]
    );
    assert_eq!(
        questions(&faq::list_all(&mut conn).await.unwrap()),
        ["A", "B", "C"]
    );
    assert!(!faq::get(&mut conn, b.id).await.unwrap().published);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn reorder_sets_every_position(mut conn: PoolConnection<Postgres>) {
    empty(&mut conn).await;
    let clock = FakeClock::new(ts("2026-12-01T09:00:00Z"));
    let a = faq::create(&mut conn, &clock, &input("A")).await.unwrap();
    let b = faq::create(&mut conn, &clock, &input("B")).await.unwrap();
    let c = faq::create(&mut conn, &clock, &input("C")).await.unwrap();

    faq::reorder(&mut conn, &clock, &[c.id, a.id, b.id])
        .await
        .unwrap();
    let all = faq::list_all(&mut conn).await.unwrap();
    assert_eq!(questions(&all), ["C", "A", "B"]);
    assert_eq!(
        all.iter().map(|entry| entry.position).collect::<Vec<_>>(),
        [0, 1, 2]
    );

    for wrong in [
        vec![a.id, b.id],
        vec![a.id, b.id, c.id, FaqId::new(999)],
        vec![a.id, a.id, b.id],
        vec![a.id, b.id, FaqId::new(999)],
    ] {
        assert!(
            matches!(
                faq::reorder(&mut conn, &clock, &wrong).await,
                Err(Error::Invalid("order"))
            ),
            "{wrong:?}"
        );
    }
    assert_eq!(
        questions(&faq::list_all(&mut conn).await.unwrap()),
        ["C", "A", "B"]
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn update_changes_the_text_and_keeps_the_place(mut conn: PoolConnection<Postgres>) {
    empty(&mut conn).await;
    let clock = FakeClock::new(ts("2026-12-01T09:00:00Z"));
    faq::create(&mut conn, &clock, &input("A")).await.unwrap();
    let b = faq::create(&mut conn, &clock, &input("B")).await.unwrap();
    clock.advance(SignedDuration::from_hours(1));

    let updated = faq::update(
        &mut conn,
        &clock,
        b.id,
        &FaqInput {
            question: "Nieuwe vraag?".to_owned(),
            answer_md: "Nieuw **antwoord**".to_owned(),
        },
    )
    .await
    .unwrap();
    assert_eq!(updated.question, "Nieuwe vraag?");
    assert_eq!(updated.answer_md, "Nieuw **antwoord**");
    assert_eq!(updated.position, 1);
    assert_eq!(updated.created_at, ts("2026-12-01T09:00:00Z"));
    assert_eq!(updated.updated_at, ts("2026-12-01T10:00:00Z"));
    assert_eq!(faq::get(&mut conn, b.id).await.unwrap(), updated);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn empty_text_and_unknown_ids_are_refused(mut conn: PoolConnection<Postgres>) {
    let clock = FakeClock::new(ts("2026-12-01T09:00:00Z"));
    let blank_question = FaqInput {
        question: " ".to_owned(),
        answer_md: "Antwoord".to_owned(),
    };
    let blank_answer = FaqInput {
        question: "Vraag".to_owned(),
        answer_md: "\n".to_owned(),
    };
    assert!(matches!(
        faq::create(&mut conn, &clock, &blank_question).await,
        Err(Error::Invalid("question"))
    ));
    assert!(matches!(
        faq::create(&mut conn, &clock, &blank_answer).await,
        Err(Error::Invalid("answer"))
    ));
    let missing = FaqId::new(999);
    assert!(matches!(
        faq::update(&mut conn, &clock, missing, &input("X")).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        faq::set_published(&mut conn, &clock, missing, true).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        faq::get(&mut conn, missing).await,
        Err(Error::NotFound)
    ));
}
