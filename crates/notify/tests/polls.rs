//! F9b: the bread vote, once per confirmed email address, closing at its set time.
#![forbid(unsafe_code)]

use domain::{BakeDayId, Clock, PollId, PollOptionId};
use i18n::Translator;
use jiff::{SignedDuration, Timestamp};
use notify::polls::{
    self, Ballot, CONFIRM_VALID, Confirmed, Invalid, KEEP_ADDRESS, Link, NewOption, PollError,
    PollInput, RESEND_AFTER, Voted,
};
use notify::subscribers::Links;
use notify::{Company, Composer};
use sqlx::pool::PoolConnection;
use sqlx::{PgConnection, Postgres};
use testkit::FakeClock;

const NOW: &str = "2026-11-24T10:00:00Z";

fn now() -> FakeClock {
    FakeClock::new(NOW.parse::<Timestamp>().unwrap())
}

fn company() -> Company {
    Company {
        name: "Brood voor Weldadigheid".to_owned(),
        address: "Majoor van Swietenlaan 15, 8382 CE Frederiksoord".to_owned(),
        kvk: "12345678".to_owned(),
    }
}

fn links() -> Links {
    Links::new("https://broodvoorweldadigheid.nl")
}

fn input(closes_in: SignedDuration) -> PollInput {
    PollInput {
        title: "Welk brood bakken we in december?".to_owned(),
        bake_day: None,
        closes_at: NOW.parse::<Timestamp>().unwrap() + closes_in,
        public_results: false,
    }
}

fn option(name: &str) -> NewOption {
    NewOption {
        product: None,
        name: name.to_owned(),
    }
}

async fn poll(conn: &mut PgConnection, clock: &FakeClock) -> (PollId, PollOptionId, PollOptionId) {
    let id = polls::create(
        conn,
        clock,
        &input(SignedDuration::from_hours(48)),
        &[option("Spelt"), option("Rogge")],
    )
    .await
    .unwrap();
    let poll = polls::get(conn, id).await.unwrap();
    (id, poll.options[0].id, poll.options[1].id)
}

async fn vote(
    conn: &mut PgConnection,
    clock: &FakeClock,
    poll: PollId,
    option: PollOptionId,
    email: &str,
    wish: &str,
) -> Result<Voted, PollError> {
    let i18n = Translator::dutch().unwrap();
    let company = company();
    polls::vote(
        conn,
        clock,
        &Composer::new(&i18n, &company),
        &links(),
        poll,
        Ballot { option, wish },
        email,
    )
    .await
}

async fn confirmations(conn: &mut PgConnection) -> Vec<(String, String)> {
    sqlx::query_as(
        "SELECT to_address, text_body FROM email_outbox
        WHERE kind = 'vote_confirmation' ORDER BY id",
    )
    .fetch_all(conn)
    .await
    .unwrap()
}

async fn last_token(conn: &mut PgConnection) -> String {
    let text = confirmations(conn).await.pop().expect("an email").1;
    let path = "https://broodvoorweldadigheid.nl/stemmen/bevestig/";
    let start = text.find(path).expect("confirm link") + path.len();
    text[start..start + 43].to_owned()
}

async fn counts(conn: &mut PgConnection, poll: PollId) -> Vec<i64> {
    polls::results(conn, poll)
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.votes)
        .collect()
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_vote_counts_once_its_address_is_confirmed(mut conn: PoolConnection<Postgres>) {
    let clock = now();
    let (id, spelt, _) = poll(&mut conn, &clock).await;
    assert_eq!(
        vote(
            &mut conn,
            &clock,
            id,
            spelt,
            " Iemand@Example.nl ",
            "Meer spelt!"
        )
        .await
        .unwrap(),
        Voted::ConfirmationQueued
    );
    let sent = confirmations(&mut conn).await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "Iemand@Example.nl");
    assert!(sent[0].1.contains("Spelt"), "{}", sent[0].1);
    assert_eq!(counts(&mut conn, id).await, [0, 0], "not before confirming");
    assert!(polls::wishes(&mut conn, id).await.unwrap().is_empty());

    let token = last_token(&mut conn).await;
    assert_eq!(
        polls::confirm_link(&mut conn, &clock, &token)
            .await
            .unwrap(),
        Link::Open {
            choice: "Spelt".to_owned(),
            wish: Some("Meer spelt!".to_owned()),
        }
    );
    let stored: Vec<u8> = sqlx::query_scalar("SELECT confirm_token_hash FROM vote")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_ne!(stored, token.as_bytes(), "only the hash is stored");
    assert_eq!(
        polls::confirm(&mut conn, &clock, &token).await.unwrap(),
        Confirmed::Counted(id)
    );
    assert_eq!(counts(&mut conn, id).await, [1, 0]);
    assert_eq!(polls::wishes(&mut conn, id).await.unwrap(), ["Meer spelt!"]);
    assert_eq!(
        polls::confirm(&mut conn, &clock, &token).await.unwrap(),
        Confirmed::Unknown,
        "the link works once"
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn one_vote_per_confirmed_address_per_poll(mut conn: PoolConnection<Postgres>) {
    let clock = now();
    let (id, spelt, rogge) = poll(&mut conn, &clock).await;
    vote(&mut conn, &clock, id, spelt, "iemand@example.nl", "")
        .await
        .unwrap();
    let token = last_token(&mut conn).await;
    polls::confirm(&mut conn, &clock, &token).await.unwrap();

    clock.advance(RESEND_AFTER * 2);
    assert_eq!(
        vote(&mut conn, &clock, id, rogge, "IEMAND@example.nl", "")
            .await
            .unwrap(),
        Voted::AlreadyVoted
    );
    assert_eq!(confirmations(&mut conn).await.len(), 1, "no new email");
    assert_eq!(counts(&mut conn, id).await, [1, 0], "the first vote stays");

    let duplicate = sqlx::query(
        "INSERT INTO vote (poll_id, poll_option_id, email, confirmed_at, created_at)
        VALUES ($1, $2, 'Iemand@Example.NL', now(), now())",
    )
    .bind(id.get())
    .bind(rogge.get())
    .execute(&mut *conn)
    .await;
    assert!(duplicate.is_err(), "the database refuses a second vote too");

    assert_eq!(
        vote(&mut conn, &clock, id, rogge, "ander@example.nl", "")
            .await
            .unwrap(),
        Voted::ConfirmationQueued
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_new_vote_while_waiting_replaces_the_old_one(mut conn: PoolConnection<Postgres>) {
    let clock = now();
    let (id, spelt, rogge) = poll(&mut conn, &clock).await;
    vote(&mut conn, &clock, id, spelt, "iemand@example.nl", "")
        .await
        .unwrap();
    let first = last_token(&mut conn).await;
    assert_eq!(
        vote(&mut conn, &clock, id, rogge, "iemand@example.nl", "")
            .await
            .unwrap(),
        Voted::RecentlySent
    );
    assert_eq!(confirmations(&mut conn).await.len(), 1);

    clock.advance(RESEND_AFTER);
    assert_eq!(
        vote(&mut conn, &clock, id, rogge, "iemand@example.nl", "Desem")
            .await
            .unwrap(),
        Voted::ConfirmationQueued
    );
    assert_eq!(
        polls::confirm(&mut conn, &clock, &first).await.unwrap(),
        Confirmed::Unknown,
        "the old link is void"
    );
    let second = last_token(&mut conn).await;
    polls::confirm(&mut conn, &clock, &second).await.unwrap();
    assert_eq!(counts(&mut conn, id).await, [0, 1]);
    assert_eq!(polls::wishes(&mut conn, id).await.unwrap(), ["Desem"]);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_poll_closes_at_its_set_time(mut conn: PoolConnection<Postgres>) {
    let clock = now();
    let (id, spelt, _) = poll(&mut conn, &clock).await;
    vote(&mut conn, &clock, id, spelt, "laat@example.nl", "")
        .await
        .unwrap();
    let token = last_token(&mut conn).await;
    let expires: Timestamp = sqlx::query_scalar::<_, String>(
        "SELECT to_char(confirm_expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')
        FROM vote",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap()
    .parse()
    .unwrap();
    assert_eq!(
        expires,
        polls::get(&mut conn, id).await.unwrap().closes_at,
        "a link never outlives its poll"
    );
    assert!(CONFIRM_VALID > SignedDuration::from_hours(48));

    clock.advance(SignedDuration::from_hours(48) - SignedDuration::from_secs(1));
    assert!(
        polls::get(&mut conn, id)
            .await
            .unwrap()
            .is_open(clock.now())
    );
    clock.advance(SignedDuration::from_secs(1));
    assert!(
        !polls::get(&mut conn, id)
            .await
            .unwrap()
            .is_open(clock.now())
    );
    assert!(matches!(
        vote(&mut conn, &clock, id, spelt, "nieuw@example.nl", "").await,
        Err(PollError::Closed)
    ));
    assert!(matches!(
        polls::vote_verified(
            &mut conn,
            &clock,
            id,
            Ballot {
                option: spelt,
                wish: ""
            },
            "klant@example.nl"
        )
        .await,
        Err(PollError::Closed)
    ));
    assert_eq!(
        polls::confirm(&mut conn, &clock, &token).await.unwrap(),
        Confirmed::Unknown
    );
    assert_eq!(counts(&mut conn, id).await, [0, 0]);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn closing_early_refuses_a_pending_confirmation(mut conn: PoolConnection<Postgres>) {
    let clock = now();
    let (id, spelt, _) = poll(&mut conn, &clock).await;
    vote(&mut conn, &clock, id, spelt, "iemand@example.nl", "")
        .await
        .unwrap();
    let token = last_token(&mut conn).await;
    let mut closed = input(SignedDuration::ZERO);
    closed.closes_at = clock.now() - SignedDuration::from_secs(1);
    polls::update(&mut conn, &clock, id, &closed).await.unwrap();
    assert_eq!(
        polls::confirm_link(&mut conn, &clock, &token)
            .await
            .unwrap(),
        Link::Closed(id)
    );
    assert_eq!(
        polls::confirm(&mut conn, &clock, &token).await.unwrap(),
        Confirmed::Closed(id)
    );
    assert_eq!(counts(&mut conn, id).await, [0, 0]);
    let reopened =
        polls::update(&mut conn, &clock, id, &input(SignedDuration::from_hours(1))).await;
    assert!(
        matches!(reopened, Err(PollError::Invalid(Invalid::Reopen))),
        "an erase may have blanked an address in it, so it could vote twice"
    );
    assert!(
        !polls::get(&mut conn, id)
            .await
            .unwrap()
            .is_open(clock.now())
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn a_signed_in_customer_votes_without_a_confirmation(mut conn: PoolConnection<Postgres>) {
    let clock = now();
    let (id, spelt, rogge) = poll(&mut conn, &clock).await;
    vote(&mut conn, &clock, id, rogge, "klant@example.nl", "")
        .await
        .unwrap();
    let ballot = Ballot {
        option: spelt,
        wish: "Volkoren",
    };
    assert_eq!(
        polls::vote_verified(&mut conn, &clock, id, ballot, "Klant@example.nl")
            .await
            .unwrap(),
        Voted::Counted
    );
    assert_eq!(
        counts(&mut conn, id).await,
        [1, 0],
        "replaces the waiting vote"
    );
    assert_eq!(
        polls::vote_verified(&mut conn, &clock, id, ballot, "klant@example.nl")
            .await
            .unwrap(),
        Voted::AlreadyVoted
    );
    assert_eq!(confirmations(&mut conn).await.len(), 1);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn bad_votes_are_refused(mut conn: PoolConnection<Postgres>) {
    let clock = now();
    let (id, spelt, _) = poll(&mut conn, &clock).await;
    let (other, other_option, _) = poll(&mut conn, &clock).await;
    assert!(matches!(
        vote(&mut conn, &clock, id, spelt, "geen-adres", "").await,
        Err(PollError::Address)
    ));
    assert!(matches!(
        vote(
            &mut conn,
            &clock,
            id,
            spelt,
            "a@example.nl",
            &"x".repeat(501)
        )
        .await,
        Err(PollError::Wish)
    ));
    assert!(matches!(
        vote(&mut conn, &clock, id, other_option, "a@example.nl", "").await,
        Err(PollError::Choice)
    ));
    assert!(matches!(
        vote(
            &mut conn,
            &clock,
            PollId::new(other.get() + 100),
            spelt,
            "a@example.nl",
            ""
        )
        .await,
        Err(PollError::NotFound)
    ));
    assert!(confirmations(&mut conn).await.is_empty());
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn polls_are_checked_when_created(mut conn: PoolConnection<Postgres>) {
    let clock = now();
    let past = polls::create(
        &mut conn,
        &clock,
        &input(SignedDuration::ZERO),
        &[option("Spelt"), option("Rogge")],
    )
    .await;
    assert!(matches!(
        past,
        Err(PollError::Invalid(Invalid::ClosesInPast))
    ));
    let one = polls::create(
        &mut conn,
        &clock,
        &input(SignedDuration::from_hours(1)),
        &[option("Spelt")],
    )
    .await;
    assert!(matches!(
        one,
        Err(PollError::Invalid(Invalid::TooFewOptions))
    ));

    let day: i64 = sqlx::query_scalar(
        "INSERT INTO bake_day (date, bake_starts_at, online_ordering)
        VALUES ('2026-12-12', now(), false) RETURNING id",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    let mut tied = input(SignedDuration::from_hours(1));
    tied.bake_day = Some(BakeDayId::new(day));
    tied.public_results = true;
    let id = polls::create(
        &mut conn,
        &clock,
        &tied,
        &[option("Spelt"), option("Rogge")],
    )
    .await
    .unwrap();
    let poll = polls::get(&mut conn, id).await.unwrap();
    assert_eq!(poll.bake_date, Some(jiff::civil::date(2026, 12, 12)));
    assert_eq!(
        poll.options
            .iter()
            .map(|o| o.name.as_str())
            .collect::<Vec<_>>(),
        ["Spelt", "Rogge"]
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn the_site_lists_open_polls_and_closed_ones_with_public_results(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = now();
    let (open, ..) = poll(&mut conn, &clock).await;
    let (private, ..) = poll(&mut conn, &clock).await;
    let (public, ..) = poll(&mut conn, &clock).await;
    let mut settings = input(SignedDuration::ZERO);
    settings.closes_at = clock.now() + SignedDuration::from_hours(1);
    polls::update(&mut conn, &clock, private, &settings)
        .await
        .unwrap();
    settings.public_results = true;
    polls::update(&mut conn, &clock, public, &settings)
        .await
        .unwrap();
    clock.advance(SignedDuration::from_hours(2));
    let shown: Vec<PollId> = polls::list_public(&mut conn, clock.now())
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(shown, [open, public]);
    assert_eq!(polls::list(&mut conn).await.unwrap().len(), 3);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn erasing_an_address_keeps_closed_votes_counting_without_it(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = now();
    let (id, spelt, rogge) = poll(&mut conn, &clock).await;
    vote(&mut conn, &clock, id, spelt, "iemand@example.nl", "Spelt!")
        .await
        .unwrap();
    let token = last_token(&mut conn).await;
    polls::confirm(&mut conn, &clock, &token).await.unwrap();
    let (other, ..) = poll(&mut conn, &clock).await;
    let other_option = polls::get(&mut conn, other).await.unwrap().options[1].id;
    vote(
        &mut conn,
        &clock,
        other,
        other_option,
        "Iemand@example.nl",
        "Rogge?",
    )
    .await
    .unwrap();
    vote(&mut conn, &clock, id, rogge, "ander@example.nl", "")
        .await
        .unwrap();

    let (still_open, open_option, _) = poll(&mut conn, &clock).await;
    let ballot = Ballot {
        option: open_option,
        wish: "",
    };
    polls::vote_verified(&mut conn, &clock, still_open, ballot, "iemand@example.nl")
        .await
        .unwrap();
    let mut closed = input(SignedDuration::ZERO);
    closed.closes_at = clock.now() - SignedDuration::from_secs(1);
    polls::update(&mut conn, &clock, id, &closed).await.unwrap();

    let mine = polls::of_address(&mut conn, "IEMAND@example.nl")
        .await
        .unwrap();
    assert_eq!(mine.len(), 3);
    assert_eq!(mine[0].choice, "Spelt");
    assert_eq!(mine[0].wish.as_deref(), Some("Spelt!"));
    assert!(mine[0].confirmed_at.is_some());
    assert!(mine[1].confirmed_at.is_none());

    polls::forget(&mut conn, &clock, "iemand@example.nl")
        .await
        .unwrap();
    assert!(
        polls::of_address(&mut conn, "iemand@example.nl")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        counts(&mut conn, id).await,
        [1, 0],
        "the confirmed vote in the closed poll still counts"
    );
    assert_eq!(
        counts(&mut conn, still_open).await,
        [0, 0],
        "a vote in an open poll is deleted"
    );
    assert_eq!(
        polls::vote_verified(&mut conn, &clock, still_open, ballot, "iemand@example.nl")
            .await
            .unwrap(),
        Voted::Counted,
        "and the address can vote there again, once"
    );
    assert!(
        polls::wishes(&mut conn, id).await.unwrap().is_empty(),
        "its wish is gone"
    );
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM vote WHERE poll_id = $1")
        .bind(other.get())
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(left, 0, "the unconfirmed vote is deleted");
    assert_eq!(
        polls::of_address(&mut conn, "ander@example.nl")
            .await
            .unwrap()
            .len(),
        1
    );
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn unconfirmed_votes_are_deleted_once_their_link_expires(mut conn: PoolConnection<Postgres>) {
    let clock = now();
    let (id, spelt, _) = poll(&mut conn, &clock).await;
    vote(&mut conn, &clock, id, spelt, "wacht@example.nl", "x")
        .await
        .unwrap();
    vote(&mut conn, &clock, id, spelt, "telt@example.nl", "")
        .await
        .unwrap();
    let token = last_token(&mut conn).await;
    polls::confirm(&mut conn, &clock, &token).await.unwrap();
    assert_eq!(polls::delete_expired(&mut conn, &clock).await.unwrap(), 0);
    clock.advance(SignedDuration::from_hours(48));
    assert_eq!(polls::delete_expired(&mut conn, &clock).await.unwrap(), 1);
    assert_eq!(counts(&mut conn, id).await, [1, 0]);
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn confirmed_votes_lose_their_address_30_days_after_the_poll_closed(
    mut conn: PoolConnection<Postgres>,
) {
    let clock = now();
    let (id, spelt, rogge) = poll(&mut conn, &clock).await;
    for (option, email, wish) in [
        (spelt, "een@example.nl", "Spelt!"),
        (rogge, "twee@example.nl", ""),
    ] {
        let ballot = Ballot { option, wish };
        polls::vote_verified(&mut conn, &clock, id, ballot, email)
            .await
            .unwrap();
    }
    let closes_at = polls::get(&mut conn, id).await.unwrap().closes_at;
    assert_eq!(KEEP_ADDRESS, SignedDuration::from_hours(30 * 24));

    clock.set(closes_at + SignedDuration::from_hours(29 * 24));
    let mut open = input(SignedDuration::ZERO);
    open.closes_at = clock.now() + SignedDuration::from_hours(48);
    let still_open = polls::create(
        &mut conn,
        &clock,
        &open,
        &[option("Spelt"), option("Rogge")],
    )
    .await
    .unwrap();
    let open_option = polls::get(&mut conn, still_open).await.unwrap().options[0].id;
    let ballot = Ballot {
        option: open_option,
        wish: "Meer zuur",
    };
    polls::vote_verified(&mut conn, &clock, still_open, ballot, "een@example.nl")
        .await
        .unwrap();

    assert_eq!(polls::blank_old(&mut conn, &clock).await.unwrap(), 0);
    assert_eq!(
        polls::of_address(&mut conn, "een@example.nl")
            .await
            .unwrap()
            .len(),
        2,
        "29 days after closing the address stays"
    );

    clock.set(closes_at + SignedDuration::from_hours(31 * 24));
    assert_eq!(polls::blank_old(&mut conn, &clock).await.unwrap(), 2);
    assert_eq!(counts(&mut conn, id).await, [1, 1], "the counts stay");
    assert!(polls::wishes(&mut conn, id).await.unwrap().is_empty());
    assert!(
        polls::of_address(&mut conn, "twee@example.nl")
            .await
            .unwrap()
            .is_empty()
    );
    let mine = polls::of_address(&mut conn, "een@example.nl")
        .await
        .unwrap();
    assert_eq!(mine.len(), 1, "the vote in the open poll keeps its address");
    assert_eq!(mine[0].wish.as_deref(), Some("Meer zuur"));

    assert_eq!(
        polls::blank_old(&mut conn, &clock).await.unwrap(),
        0,
        "a second run changes nothing"
    );
    assert_eq!(counts(&mut conn, id).await, [1, 1]);
}
