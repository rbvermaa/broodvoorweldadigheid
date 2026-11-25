//! Instructions of a recipe version (F14, ADR 0049): none, a free text, or steps. They
//! belong to the version like its lines: only a draft changes, and a new version copies
//! them. They never affect allergens, labels or costing.

use std::fmt;
use std::str::FromStr;

use domain::RecipeVersionId;
use sqlx::{Acquire, PgConnection};

use crate::Error;
use crate::formula::UnknownCode;
use crate::recipes::lock_draft;

pub const MAX_STEPS: usize = 40;
/// In characters, as are the other lengths.
pub const MAX_STEP_NAME: usize = 80;
pub const MAX_STEP_NOTES: usize = 1000;
pub const MAX_TEXT: usize = 10_000;
/// A week, in minutes.
pub const MAX_DURATION_MIN: u32 = 10_080;
pub const MIN_TEMPERATURE_C: i32 = -30;
pub const MAX_TEMPERATURE_C: i32 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MethodKind {
    None,
    Text,
    Steps,
}

impl MethodKind {
    pub const ALL: [Self; 3] = [Self::None, Self::Text, Self::Steps];

    /// Stored in the database; never change.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Text => "text",
            Self::Steps => "steps",
        }
    }
}

impl FromStr for MethodKind {
    type Err = UnknownCode;

    fn from_str(code: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.code() == code)
            .ok_or_else(|| UnknownCode(code.to_owned()))
    }
}

impl fmt::Display for MethodKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Step {
    pub name: String,
    pub duration_min: Option<u32>,
    pub temperature_c: Option<i32>,
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method {
    None,
    Text(String),
    Steps(Vec<Step>),
}

impl Method {
    #[must_use]
    pub const fn kind(&self) -> MethodKind {
        match self {
            Self::None => MethodKind::None,
            Self::Text(_) => MethodKind::Text,
            Self::Steps(_) => MethodKind::Steps,
        }
    }

    /// Nothing would be lost by switching to another kind.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Self::None => true,
            Self::Text(text) => text.is_empty(),
            Self::Steps(steps) => steps.is_empty(),
        }
    }
}

/// Text as stored: line ends as `\n`, no blank space around it.
#[must_use]
pub fn clean_text(text: &str) -> String {
    text.replace("\r\n", "\n").trim().to_owned()
}

/// # Errors
/// [`Error::Invalid`]: `"step_name"`, `"duration"`, `"temperature"` or `"step_notes"`.
pub fn validate_step(step: &Step) -> Result<(), Error> {
    let name = step.name.trim();
    if name.is_empty() || name.chars().count() > MAX_STEP_NAME || name != step.name {
        return Err(Error::Invalid("step_name"));
    }
    if step
        .duration_min
        .is_some_and(|d| !(1..=MAX_DURATION_MIN).contains(&d))
    {
        return Err(Error::Invalid("duration"));
    }
    if step
        .temperature_c
        .is_some_and(|t| !(MIN_TEMPERATURE_C..=MAX_TEMPERATURE_C).contains(&t))
    {
        return Err(Error::Invalid("temperature"));
    }
    if step.notes.chars().count() > MAX_STEP_NOTES || clean_text(&step.notes) != step.notes {
        return Err(Error::Invalid("step_notes"));
    }
    Ok(())
}

/// A short fingerprint of the instructions (FNV-1a, 64 bits). A page sends back the stamp
/// of what it showed, so a change made on an out-of-date page is refused.
#[must_use]
pub fn stamp(method: &Method) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for byte in bytes.iter().chain(&[0xff]) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    };
    eat(method.kind().code().as_bytes());
    match method {
        Method::None => {}
        Method::Text(text) => eat(text.as_bytes()),
        Method::Steps(steps) => {
            for step in steps {
                eat(step.name.as_bytes());
                eat(&step.duration_min.map_or(-1, i64::from).to_le_bytes());
                eat(&step.temperature_c.map_or(i64::MIN, i64::from).to_le_bytes());
                eat(step.notes.as_bytes());
            }
        }
    }
    format!("{hash:016x}")
}

/// The instructions of a version.
///
/// # Errors
/// [`Error::NotFound`] for an unknown version, [`Error::Corrupt`] for bad stored values.
pub async fn method(conn: &mut PgConnection, id: RecipeVersionId) -> Result<Method, Error> {
    let row = sqlx::query!(
        "SELECT method_kind, method_text FROM recipe_version WHERE id = $1",
        id.get(),
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(Error::NotFound)?;
    Ok(match row.method_kind.parse::<MethodKind>()? {
        MethodKind::None => Method::None,
        MethodKind::Text => Method::Text(row.method_text),
        MethodKind::Steps => Method::Steps(steps(conn, id).await?),
    })
}

async fn steps(conn: &mut PgConnection, id: RecipeVersionId) -> Result<Vec<Step>, Error> {
    sqlx::query!(
        "SELECT name, duration_min, temperature_c, notes FROM recipe_step
        WHERE recipe_version_id = $1 ORDER BY position",
        id.get(),
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| {
        Ok(Step {
            name: row.name,
            duration_min: row
                .duration_min
                .map(u32::try_from)
                .transpose()
                .map_err(|_| Error::Corrupt("negative step duration".to_owned()))?,
            temperature_c: row.temperature_c,
            notes: row.notes,
        })
    })
    .collect()
}

/// Locks a draft and returns its instructions after checking them against `seen`.
async fn lock(conn: &mut PgConnection, id: RecipeVersionId, seen: &str) -> Result<Method, Error> {
    lock_draft(conn, id).await?;
    let current = method(conn, id).await?;
    if stamp(&current) != seen {
        return Err(Error::Stale);
    }
    Ok(current)
}

async fn write_kind(
    conn: &mut PgConnection,
    id: RecipeVersionId,
    kind: MethodKind,
    text: &str,
) -> Result<(), Error> {
    sqlx::query!(
        "DELETE FROM recipe_step WHERE recipe_version_id = $1",
        id.get()
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "UPDATE recipe_version SET method_kind = $2, method_text = $3 WHERE id = $1",
        id.get(),
        kind.code(),
        text,
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Switches a draft's instructions to another kind; the content of the old kind goes.
/// `seen` is the [`stamp`] of the instructions the switch was asked on.
///
/// # Errors
/// [`Error::NotFound`], [`Error::Frozen`], or [`Error::Stale`] when the instructions
/// changed since `seen`.
pub async fn set_kind(
    conn: &mut PgConnection,
    id: RecipeVersionId,
    seen: &str,
    kind: MethodKind,
) -> Result<Method, Error> {
    let mut tx = conn.begin().await?;
    let current = lock(&mut tx, id, seen).await?;
    if current.kind() != kind {
        write_kind(&mut tx, id, kind, "").await?;
    }
    let saved = method(&mut tx, id).await?;
    tx.commit().await?;
    Ok(saved)
}

/// Saves the free text of a draft whose instructions are text.
///
/// # Errors
/// [`Error::NotFound`], [`Error::Frozen`], [`Error::Stale`] when the draft's instructions
/// are not text, or [`Error::Invalid`] (`"method_text"`) for a text that is too long.
pub async fn save_text(
    conn: &mut PgConnection,
    id: RecipeVersionId,
    text: &str,
) -> Result<Method, Error> {
    let text = clean_text(text);
    if text.chars().count() > MAX_TEXT {
        return Err(Error::Invalid("method_text"));
    }
    let mut tx = conn.begin().await?;
    lock_draft(&mut tx, id).await?;
    if method(&mut tx, id).await?.kind() != MethodKind::Text {
        return Err(Error::Stale);
    }
    write_kind(&mut tx, id, MethodKind::Text, &text).await?;
    let saved = method(&mut tx, id).await?;
    tx.commit().await?;
    Ok(saved)
}

/// One change to the steps of a draft. `position` counts from 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepEdit {
    Set {
        position: usize,
        step: Step,
    },
    /// Adds the step at the end.
    Add(Step),
    Remove {
        position: usize,
    },
    /// Swaps the step with the one before it.
    MoveUp {
        position: usize,
    },
    /// Swaps the step with the one after it.
    MoveDown {
        position: usize,
    },
}

fn apply(steps: &mut Vec<Step>, edit: StepEdit) -> Result<(), Error> {
    let gone = Error::Invalid("step");
    let len = steps.len();
    match edit {
        StepEdit::Set { position, step } => {
            *steps.get_mut(position).ok_or(gone)? = step;
        }
        StepEdit::Add(step) => steps.push(step),
        StepEdit::Remove { position } if position < len => {
            steps.remove(position);
        }
        StepEdit::MoveUp { position } if (1..len).contains(&position) => {
            steps.swap(position - 1, position);
        }
        StepEdit::MoveDown { position } if position < len.saturating_sub(1) => {
            steps.swap(position, position + 1);
        }
        StepEdit::Remove { .. } | StepEdit::MoveUp { .. } | StepEdit::MoveDown { .. } => {
            return Err(gone);
        }
    }
    Ok(())
}

/// Changes the steps of a draft whose instructions are steps, one edit at a time. `seen`
/// is the [`stamp`] of the instructions the change was made on.
///
/// # Errors
/// [`Error::NotFound`], [`Error::Frozen`], [`Error::Stale`] when the instructions changed
/// since `seen` (or are not steps), [`Error::Invalid`]: `"step"` for a position with no
/// step, `"steps"` for more than [`MAX_STEPS`], or as for [`validate_step`].
pub async fn edit_step(
    conn: &mut PgConnection,
    id: RecipeVersionId,
    seen: &str,
    edit: StepEdit,
) -> Result<Method, Error> {
    if let StepEdit::Set { step, .. } | StepEdit::Add(step) = &edit {
        validate_step(step)?;
    }
    let mut tx = conn.begin().await?;
    let Method::Steps(mut new_steps) = lock(&mut tx, id, seen).await? else {
        return Err(Error::Stale);
    };
    apply(&mut new_steps, edit)?;
    if new_steps.len() > MAX_STEPS {
        return Err(Error::Invalid("steps"));
    }
    replace_steps(&mut tx, id, &new_steps).await?;
    let saved = method(&mut tx, id).await?;
    tx.commit().await?;
    Ok(saved)
}

async fn replace_steps(
    conn: &mut PgConnection,
    id: RecipeVersionId,
    steps: &[Step],
) -> Result<(), Error> {
    sqlx::query!(
        "DELETE FROM recipe_step WHERE recipe_version_id = $1",
        id.get()
    )
    .execute(&mut *conn)
    .await?;
    let positions: Vec<i32> = (0..steps.len())
        .map(|p| i32::try_from(p).unwrap_or(i32::MAX))
        .collect();
    let names: Vec<String> = steps.iter().map(|s| s.name.clone()).collect();
    let durations: Vec<Option<i32>> = steps
        .iter()
        .map(|s| s.duration_min.map(|d| i32::try_from(d).unwrap_or(i32::MAX)))
        .collect();
    let temperatures: Vec<Option<i32>> = steps.iter().map(|s| s.temperature_c).collect();
    let notes: Vec<String> = steps.iter().map(|s| s.notes.clone()).collect();
    sqlx::query!(
        "INSERT INTO recipe_step
            (recipe_version_id, position, name, duration_min, temperature_c, notes)
        SELECT $1, t.position, t.name, t.duration_min, t.temperature_c, t.notes
        FROM unnest($2::int[], $3::text[], $4::int[], $5::int[], $6::text[])
            AS t (position, name, duration_min, temperature_c, notes)",
        id.get(),
        &positions,
        &names,
        &durations as &[Option<i32>],
        &temperatures as &[Option<i32>],
        &notes,
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Gives the draft `to` the instructions of version `from`, for a new version.
pub(crate) async fn copy(
    conn: &mut PgConnection,
    from: RecipeVersionId,
    to: RecipeVersionId,
) -> Result<(), Error> {
    sqlx::query!(
        "UPDATE recipe_version t SET method_kind = f.method_kind, method_text = f.method_text
        FROM recipe_version f WHERE f.id = $1 AND t.id = $2",
        from.get(),
        to.get(),
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "INSERT INTO recipe_step
            (recipe_version_id, position, name, duration_min, temperature_c, notes)
        SELECT $2, position, name, duration_min, temperature_c, notes
        FROM recipe_step WHERE recipe_version_id = $1",
        from.get(),
        to.get(),
    )
    .execute(conn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(name: &str) -> Step {
        Step {
            name: name.to_owned(),
            ..Step::default()
        }
    }

    #[test]
    fn kinds_round_trip_through_their_codes() {
        for kind in MethodKind::ALL {
            assert_eq!(kind.code().parse::<MethodKind>().unwrap(), kind);
            assert_eq!(kind.to_string(), kind.code());
        }
        assert!("tekst".parse::<MethodKind>().is_err());
    }

    #[test]
    fn steps_are_checked_field_by_field() {
        assert!(validate_step(&step("Autolyse")).is_ok());
        let cases = [
            (step(""), "step_name"),
            (step(" Bulk"), "step_name"),
            (step(&"a".repeat(MAX_STEP_NAME + 1)), "step_name"),
            (
                Step {
                    duration_min: Some(0),
                    ..step("Bulk")
                },
                "duration",
            ),
            (
                Step {
                    duration_min: Some(MAX_DURATION_MIN + 1),
                    ..step("Bulk")
                },
                "duration",
            ),
            (
                Step {
                    temperature_c: Some(MAX_TEMPERATURE_C + 1),
                    ..step("Bakken")
                },
                "temperature",
            ),
            (
                Step {
                    temperature_c: Some(MIN_TEMPERATURE_C - 1),
                    ..step("Vriezen")
                },
                "temperature",
            ),
            (
                Step {
                    notes: "x".repeat(MAX_STEP_NOTES + 1),
                    ..step("Bulk")
                },
                "step_notes",
            ),
            (
                Step {
                    notes: "vouwen\r\n".to_owned(),
                    ..step("Bulk")
                },
                "step_notes",
            ),
        ];
        for (bad, field) in cases {
            let result = validate_step(&bad);
            assert!(
                matches!(result, Err(Error::Invalid(f)) if f == field),
                "{bad:?}: {result:?}"
            );
        }
        let edge = Step {
            name: "é".repeat(MAX_STEP_NAME),
            duration_min: Some(MAX_DURATION_MIN),
            temperature_c: Some(MIN_TEMPERATURE_C),
            notes: "é".repeat(MAX_STEP_NOTES),
        };
        assert!(validate_step(&edge).is_ok());
    }

    #[test]
    fn the_stamp_changes_with_any_field() {
        let base = Method::Steps(vec![Step {
            name: "Bulk".to_owned(),
            duration_min: Some(240),
            temperature_c: Some(24),
            notes: "4 keer vouwen".to_owned(),
        }]);
        let variants = [
            Method::None,
            Method::Text(String::new()),
            Method::Steps(Vec::new()),
            Method::Steps(vec![Step {
                name: "Bulk".to_owned(),
                duration_min: Some(241),
                temperature_c: Some(24),
                notes: "4 keer vouwen".to_owned(),
            }]),
            Method::Steps(vec![Step {
                name: "Bulk".to_owned(),
                duration_min: Some(240),
                temperature_c: None,
                notes: "4 keer vouwen".to_owned(),
            }]),
            Method::Steps(vec![Step {
                name: "Bulk".to_owned(),
                duration_min: Some(240),
                temperature_c: Some(24),
                notes: String::new(),
            }]),
            Method::Steps(vec![step("Bulk"), step("")]),
        ];
        let stamps: std::collections::BTreeSet<String> =
            variants.iter().chain([&base]).map(stamp).collect();
        assert_eq!(stamps.len(), variants.len() + 1);
        assert_eq!(stamp(&base), stamp(&base.clone()));
        assert_ne!(
            stamp(&Method::Steps(vec![step("ab"), step("c")])),
            stamp(&Method::Steps(vec![step("a"), step("bc")]))
        );
    }

    #[test]
    fn emptiness_and_kind_follow_the_content() {
        assert!(Method::None.is_empty());
        assert!(Method::Text(String::new()).is_empty());
        assert!(!Method::Text("Kneden".to_owned()).is_empty());
        assert!(Method::Steps(Vec::new()).is_empty());
        assert!(!Method::Steps(vec![step("Bulk")]).is_empty());
        assert_eq!(Method::Text(String::new()).kind(), MethodKind::Text);
    }

    #[test]
    fn edits_move_and_remove_by_position() {
        let mut steps = vec![step("a"), step("b"), step("c")];
        apply(&mut steps, StepEdit::MoveUp { position: 2 }).unwrap();
        apply(&mut steps, StepEdit::MoveDown { position: 0 }).unwrap();
        let names: Vec<&str> = steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["c", "a", "b"]);
        for edit in [
            StepEdit::MoveUp { position: 0 },
            StepEdit::MoveDown { position: 2 },
            StepEdit::MoveDown {
                position: usize::MAX,
            },
            StepEdit::MoveUp {
                position: usize::MAX,
            },
            StepEdit::Remove { position: 3 },
            StepEdit::Set {
                position: 3,
                step: step("d"),
            },
        ] {
            assert!(matches!(
                apply(&mut steps, edit),
                Err(Error::Invalid("step"))
            ));
        }
        apply(&mut steps, StepEdit::Remove { position: 1 }).unwrap();
        assert_eq!(steps, [step("c"), step("b")]);
    }

    #[test]
    fn text_is_trimmed_with_plain_line_ends() {
        assert_eq!(clean_text("  Meng\r\n\r\nKneed \n"), "Meng\n\nKneed");
    }
}
