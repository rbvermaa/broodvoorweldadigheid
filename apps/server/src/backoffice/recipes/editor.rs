//! The recipe table of one version (F14): grams, name and baker's percentage per line at a
//! dough weight, and on a draft, line edits that save one line at a time through htmx.
//!
//! Every line edit keeps the grams per percent, so the other lines keep their grams and
//! the dough weight follows the change.

use std::collections::{BTreeSet, HashMap};

use askama::Template;
use auth::Permission;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use catalog::products as catalog_products;
use domain::{Allergen, IngredientId, ProductId, RecipeVersionId};
use recipes::recipes::{self as store, MAX_LEAD_TIME_HOURS, MAX_LINE_PCT};
use recipes::schedule::default_start;
use recipes::{
    Formula, Ingredient, IngredientInput, IngredientRole, Line, LineEdit, LineInput, LinePart,
    Part, Pct, Recipe, ScaleError, Scaled, SubChoice, Version, dough_for_sum, ingredients,
    leaf_grams, pct_for_grams,
};
use tower_sessions::Session;

use super::super::form::{Fields, parse_pct, pct_value};
use super::super::ingredients::role_label;
use super::super::products::AllergenBox;
use super::super::{
    Outcome, Rejection, Step, fail, invalid_status, is_htmx, layout, not_found, require, saved, t,
    t_with,
};
use super::method::{
    MethodEditor, MethodState, MethodView, Shown, editor as method_editor, load_view,
};
use super::{
    Message, date_text, grams_text, load_version, pct_text, recipe_path, status_text, sub_label,
    version_path,
};
use crate::components::{Cell, SelectOption, Table};
use crate::pages::{self, Layout};
use crate::staff::Staff;
use crate::state::AppState;

const DEFAULT_WEIGHT_G: u64 = 1000;
const MAX_WEIGHT_G: u64 = 1_000_000;
const MAX_PIECES: u64 = 10_000;
/// Suggestions the type-ahead shows at most.
const MAX_SUGGESTIONS: usize = 8;

/// A product made from the recipe, for scaling by pieces.
struct PieceProduct {
    id: ProductId,
    name: String,
    dough_g: u64,
}

/// The dough weight to show, as asked in the URL or form.
struct Scale {
    grams: u64,
    weight: String,
    weight_error: String,
    pieces: String,
    pieces_error: String,
    product: Option<ProductId>,
    /// The sub-recipe lines shown with their own lines, by position.
    open: BTreeSet<usize>,
}

impl Scale {
    fn weight(grams: u64, open: BTreeSet<usize>) -> Self {
        Self {
            grams,
            weight: grams.to_string(),
            weight_error: String::new(),
            pieces: String::new(),
            pieces_error: String::new(),
            product: None,
            open,
        }
    }

    fn read(state: &AppState, fields: &Fields, products: &[PieceProduct]) -> Self {
        let open = fields.all("open").filter_map(|v| v.parse().ok()).collect();
        let product = fields
            .text("product")
            .parse::<i64>()
            .ok()
            .and_then(|id| products.iter().find(|p| p.id.get() == id))
            .or_else(|| products.first());
        let raw_pieces = fields.text("stuks").to_owned();
        let mut pieces_error = String::new();
        if !raw_pieces.is_empty() {
            let grams = raw_pieces
                .parse::<u64>()
                .ok()
                .filter(|n| (1..=MAX_PIECES).contains(n))
                .zip(product)
                .map(|(n, p)| n * p.dough_g)
                .filter(|g| (1..=MAX_WEIGHT_G).contains(g));
            if let Some(grams) = grams {
                return Self {
                    pieces: raw_pieces,
                    product: product.map(|p| p.id),
                    ..Self::weight(grams, open)
                };
            }
            pieces_error = t(state, "recipe-pieces-error");
        }
        let raw = fields.text("gewicht");
        let parsed = raw
            .parse::<u64>()
            .ok()
            .filter(|g| (1..=MAX_WEIGHT_G).contains(g));
        let mut scale = Self::weight(parsed.unwrap_or(DEFAULT_WEIGHT_G), open);
        scale.pieces = raw_pieces;
        scale.pieces_error = pieces_error;
        if parsed.is_none() && !raw.is_empty() {
            raw.clone_into(&mut scale.weight);
            scale.weight_error = t(state, "recipe-weight-error");
        }
        scale
    }

    /// The query that shows this scale again.
    fn query(&self) -> String {
        let mut parts = if self.pieces.is_empty() || !self.pieces_error.is_empty() {
            vec![format!("gewicht={}", self.grams)]
        } else {
            let mut parts = vec![format!("stuks={}", self.pieces)];
            parts.extend(self.product.map(|p| format!("product={}", p.get())));
            parts
        };
        parts.extend(self.open.iter().map(|i| format!("open={i}")));
        parts.join("&")
    }
}

/// A line inside a sub-recipe line; read-only, its own recipe page edits it.
pub struct Child {
    pub depth: usize,
    pub name: String,
    pub icon: &'static str,
    pub icon_label: String,
    pub grams: String,
    pub pct: String,
}

/// One line of the version's own formula.
pub struct Row {
    pub index: usize,
    pub part: String,
    pub name: String,
    pub link: String,
    pub icon: &'static str,
    pub icon_label: String,
    pub grams: String,
    pub grams_value: String,
    pub pct: String,
    pub pct_value: String,
    pub warning: String,
    pub open: bool,
    pub children: Vec<Child>,
    pub action: String,
    pub remove: String,
    pub gram_label: String,
    pub pct_label: String,
    pub remove_label: String,
    pub toggle_label: String,
}

pub struct Footer {
    pub flour: String,
    pub flour_pct: String,
    pub flour_warning: String,
    pub water: String,
    pub hydration: String,
    pub dough: String,
    pub total_pct: String,
}

pub struct Suggestion {
    pub value: String,
    pub label: String,
}

/// What the type-ahead offers for the name typed so far.
pub struct Suggestions {
    pub matches: Vec<Suggestion>,
    /// The name for "Nieuw ingrediënt …"; empty when an ingredient has this name or the
    /// staff member may not set allergens.
    pub new_name: String,
    pub new_label: String,
    pub new_open: bool,
    pub roles: Vec<SelectOption>,
    pub role_error: String,
    pub allergens: Vec<AllergenBox>,
}

pub struct AddRow {
    pub action: String,
    pub search: String,
    pub q: String,
    pub pct: String,
    pub gram: String,
    pub suggestions: Suggestions,
}

pub struct Editor {
    pub editable: bool,
    pub page: String,
    /// A link to the other mode at the same scale; empty for none.
    pub switch_href: String,
    pub switch_label: String,
    pub caption: String,
    pub rows: Vec<Row>,
    pub footer: Option<Footer>,
    pub notice: Option<Message>,
    pub saved: bool,
    pub grams: String,
    /// The [`store::stamp`] of the lines shown, posted back with every line change.
    pub stamp: String,
    pub weight: String,
    pub weight_error: String,
    pub pieces: String,
    pub pieces_error: String,
    pub pieces_hint: String,
    pub can_scale_pieces: bool,
    pub products: Vec<SelectOption>,
    pub add: Option<AddRow>,
}

fn part_value(part: LinePart) -> String {
    match part {
        LinePart::Ingredient(id) => format!("i{}", id.get()),
        LinePart::Sub(id) => format!("v{}", id.get()),
    }
}

fn parse_part(value: &str) -> Option<LinePart> {
    let id = |s: &str| s.parse::<i64>().ok().filter(|id| *id > 0);
    if let Some(rest) = value.strip_prefix('i') {
        id(rest).map(|id| LinePart::Ingredient(IngredientId::new(id)))
    } else {
        value
            .strip_prefix('v')
            .and_then(id)
            .map(|id| LinePart::Sub(RecipeVersionId::new(id)))
    }
}

fn line_part(line: &Line) -> LinePart {
    match &line.part {
        Part::Ingredient(i) => LinePart::Ingredient(i.id),
        Part::Sub(s) => LinePart::Sub(s.version_id),
    }
}

fn role_icon(state: &AppState, role: IngredientRole) -> (&'static str, String) {
    match role {
        IngredientRole::Flour => ("wheat", t(state, role_label(role))),
        IngredientRole::Water => ("droplet", t(state, role_label(role))),
        IngredientRole::Other => ("", String::new()),
    }
}

/// Everything a recipe table needs from the database.
pub(super) struct Loaded {
    pub(super) recipe: Recipe,
    pub(super) version: Version,
    formula: Formula,
    frozen_subs: HashMap<RecipeVersionId, bool>,
    subs: Vec<SubChoice>,
    products: Vec<PieceProduct>,
}

pub(super) async fn load(state: &AppState, raw_id: &str, raw_version: &str) -> Step<Loaded> {
    let (recipe, version) = load_version(state, raw_id, raw_version).await?;
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let formula = store::formula(&mut conn, version.id)
        .await
        .map_err(|e| fail(state, &e))?;
    let subs = store::sub_choices(&mut conn, recipe.id)
        .await
        .map_err(|e| fail(state, &e))?;
    let linked = store::products(&mut conn, recipe.id)
        .await
        .map_err(|e| fail(state, &e))?;
    let all = catalog_products::list_all(&mut conn)
        .await
        .map_err(|e| fail(state, &e))?;
    let products = linked
        .iter()
        .filter_map(|l| all.iter().find(|p| p.id == l.id))
        .filter_map(|p| {
            u64::try_from(p.dough_g_per_piece)
                .ok()
                .filter(|g| *g > 0)
                .map(|dough_g| PieceProduct {
                    id: p.id,
                    name: p.name_nl.clone(),
                    dough_g,
                })
        })
        .collect();
    Ok(Loaded {
        frozen_subs: subs.iter().map(|s| (s.version_id, s.frozen)).collect(),
        subs,
        recipe,
        version,
        formula,
        products,
    })
}

fn contains_version(formula: &Formula, id: RecipeVersionId) -> bool {
    formula.lines.iter().any(|line| match &line.part {
        Part::Sub(sub) => sub.version_id == id || contains_version(&sub.formula, id),
        Part::Ingredient(_) => false,
    })
}

fn children(
    state: &AppState,
    lines: &[Line],
    scaled: Option<&[Scaled<'_>]>,
    depth: usize,
    out: &mut Vec<Child>,
) {
    for (index, line) in lines.iter().enumerate() {
        let scaled_line = scaled.and_then(|s| s.get(index));
        let (name, (icon, icon_label)) = match &line.part {
            Part::Ingredient(i) => (i.name.clone(), role_icon(state, i.role)),
            Part::Sub(s) => (sub_label(state, &s.name, s.version), ("", String::new())),
        };
        out.push(Child {
            depth,
            name,
            icon,
            icon_label,
            grams: scaled_line.map_or_else(|| t(state, "recipe-none"), |s| grams_text(s.grams)),
            pct: pct_text(line.pct),
        });
        if let Part::Sub(sub) = &line.part {
            children(
                state,
                &sub.formula.lines,
                scaled_line.map(|s| s.lines.as_slice()),
                depth + 1,
                out,
            );
        }
    }
}

fn sub_warning(
    state: &AppState,
    loaded: &Loaded,
    empty_sub: Option<RecipeVersionId>,
    sub: &recipes::SubRecipe,
) -> String {
    if empty_sub.is_some_and(|id| id == sub.version_id || contains_version(&sub.formula, id)) {
        t(state, "recipe-empty-sub")
    } else if loaded.frozen_subs.get(&sub.version_id) == Some(&false) {
        t_with(
            state,
            "recipe-warning-sub-draft",
            &[("name", sub.name.clone()), ("n", sub.version.to_string())],
        )
    } else {
        String::new()
    }
}

fn rows(state: &AppState, loaded: &Loaded, scale: &Scale) -> Vec<Row> {
    let formula = &loaded.formula;
    let base = version_path(loaded.recipe.id, loaded.version.id);
    let scaled = formula.scale(scale.grams);
    let empty_sub = match &scaled {
        Err(ScaleError::Empty(Some(id))) => Some(*id),
        _ => None,
    };
    let top = scaled.as_deref().ok();
    formula
        .lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let scaled_line = top.and_then(|s| s.get(index));
            let mut kids = Vec::new();
            let (name, link, (icon, icon_label), warning) = match &line.part {
                Part::Ingredient(i) => (
                    i.name.clone(),
                    String::new(),
                    role_icon(state, i.role),
                    String::new(),
                ),
                Part::Sub(sub) => {
                    children(
                        state,
                        &sub.formula.lines,
                        scaled_line.map(|s| s.lines.as_slice()),
                        1,
                        &mut kids,
                    );
                    (
                        sub_label(state, &sub.name, sub.version),
                        version_path(sub.recipe_id, sub.version_id),
                        ("", String::new()),
                        sub_warning(state, loaded, empty_sub, sub),
                    )
                }
            };
            let warning = if warning.is_empty() && scaled_line.is_some_and(|s| s.grams == 0) {
                t(state, "recipe-warning-zero")
            } else {
                warning
            };
            let named = |id: &str| t_with(state, id, &[("name", name.clone())]);
            Row {
                index,
                part: part_value(line_part(line)),
                gram_label: named("recipe-cell-grams"),
                pct_label: named("recipe-cell-pct"),
                remove_label: named("recipe-remove"),
                toggle_label: named("recipe-toggle"),
                name,
                link,
                icon,
                icon_label,
                grams: scaled_line.map_or_else(|| t(state, "recipe-none"), |s| grams_text(s.grams)),
                grams_value: scaled_line.map(|s| s.grams.to_string()).unwrap_or_default(),
                pct: pct_text(line.pct),
                pct_value: pct_value(line.pct),
                warning,
                open: scale.open.contains(&index),
                children: kids,
                action: format!("{base}/lines/{index}"),
                remove: format!("{base}/lines/{index}/remove"),
            }
        })
        .collect()
}

fn footer(state: &AppState, formula: &Formula, grams: u64) -> Option<Footer> {
    let scaled = formula.scale(grams).ok()?;
    let analysis = formula.analysis().ok()?;
    let leaves = leaf_grams(&scaled);
    Some(Footer {
        flour: grams_text(leaves.flour),
        flour_pct: pct_text(analysis.total_flour),
        flour_warning: if analysis.total_flour == Pct::HUNDRED {
            String::new()
        } else {
            t_with(
                state,
                "recipe-flour-off",
                &[("pct", pct_text(analysis.total_flour))],
            )
        },
        water: grams_text(leaves.water),
        hydration: analysis
            .hydration
            .map_or_else(|| t(state, "recipe-none"), pct_text),
        dough: grams_text(grams),
        total_pct: pct_text(Pct::from_thousandths(
            u32::try_from(formula.line_sum()).unwrap_or(u32::MAX),
        )),
    })
}

/// What an editor shows besides the formula.
struct EditorInput<'a> {
    scale: &'a Scale,
    page: String,
    /// The other mode of the page (its path without query) and the link text.
    switch: Option<(String, String)>,
    notice: Option<Message>,
    saved: bool,
    add: Option<AddRow>,
}

fn editor(state: &AppState, loaded: &Loaded, input: EditorInput<'_>) -> Editor {
    let EditorInput {
        scale,
        page,
        switch,
        mut notice,
        saved,
        add,
    } = input;
    let scale_error = [&scale.weight_error, &scale.pieces_error]
        .into_iter()
        .find(|e| !e.is_empty());
    if let (None, Some(error)) = (&notice, scale_error) {
        notice = Some(Message {
            kind: "error",
            text: error.clone(),
        });
    }
    if loaded.formula.lines.is_empty() && notice.is_none() {
        notice = Some(Message {
            kind: "info",
            text: t(state, "recipe-no-lines"),
        });
    }
    let (switch_href, switch_label) = switch
        .map(|(path, label)| (format!("{path}?{}", scale.query()), label))
        .unwrap_or_default();
    Editor {
        editable: add.is_some(),
        page,
        switch_href,
        switch_label,
        caption: t_with(
            state,
            "recipe-lines-caption",
            &[("grams", grams_text(scale.grams))],
        ),
        rows: rows(state, loaded, scale),
        footer: footer(state, &loaded.formula, scale.grams),
        notice,
        saved,
        grams: scale.grams.to_string(),
        stamp: store::stamp(
            &loaded
                .formula
                .lines
                .iter()
                .map(|line| LineInput {
                    pct: line.pct,
                    part: line_part(line),
                })
                .collect::<Vec<_>>(),
        ),
        weight: scale.weight.clone(),
        weight_error: scale.weight_error.clone(),
        pieces: scale.pieces.clone(),
        pieces_error: scale.pieces_error.clone(),
        pieces_hint: match loaded.products.as_slice() {
            [only] => t_with(
                state,
                "recipe-pieces-hint",
                &[
                    ("name", only.name.clone()),
                    ("grams", grams_text(only.dough_g)),
                ],
            ),
            _ => String::new(),
        },
        can_scale_pieces: !loaded.products.is_empty(),
        products: if loaded.products.len() > 1 {
            loaded
                .products
                .iter()
                .map(|p| SelectOption {
                    value: p.id.get().to_string(),
                    label: p.name.clone(),
                    selected: scale.product == Some(p.id),
                })
                .collect()
        } else {
            Vec::new()
        },
        add,
    }
}

/// The add row as typed, kept when a change is refused.
struct AddState {
    q: String,
    pct: String,
    gram: String,
    role: String,
    allergens: BTreeSet<Allergen>,
    new: bool,
    role_error: String,
}

impl AddState {
    fn empty() -> Self {
        Self::typed(String::new())
    }

    fn typed(q: String) -> Self {
        Self {
            q,
            pct: String::new(),
            gram: String::new(),
            role: String::new(),
            allergens: BTreeSet::new(),
            new: false,
            role_error: String::new(),
        }
    }

    fn posted(fields: &Fields) -> Self {
        Self {
            q: fields.text("q").to_owned(),
            pct: fields.text("pct").to_owned(),
            gram: fields.text("gram").to_owned(),
            role: fields.text("role").to_owned(),
            allergens: Allergen::ALL
                .into_iter()
                .filter(|a| fields.checked(&format!("allergen_{}", a.code())))
                .collect(),
            new: fields.text("new") == "1",
            role_error: String::new(),
        }
    }
}

fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

fn suggestions(
    state: &AppState,
    staff: &Staff,
    ingredients: &[Ingredient],
    subs: &[SubChoice],
    add: &AddState,
) -> Suggestions {
    let q = add.q.as_str();
    let words: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
    let hits = |label: &str| {
        let label = label.to_lowercase();
        !words.is_empty() && words.iter().all(|word| label.contains(word.as_str()))
    };
    let ingredient_hits = ingredients
        .iter()
        .filter(|i| hits(&i.name))
        .map(|i| Suggestion {
            value: part_value(LinePart::Ingredient(i.id)),
            label: i.name.clone(),
        });
    let sub_hits = subs
        .iter()
        .map(|s| {
            let id = if s.frozen {
                "recipe-sub-option"
            } else {
                "recipe-sub-option-draft"
            };
            Suggestion {
                value: part_value(LinePart::Sub(s.version_id)),
                label: t_with(
                    state,
                    id,
                    &[
                        ("name", s.recipe_name.clone()),
                        ("n", s.version.to_string()),
                    ],
                ),
            }
        })
        .filter(|s| hits(&s.label));
    let exact = ingredients.iter().any(|i| same_name(&i.name, q));
    let new_name = if q.is_empty() || exact || !staff.0.role.can(Permission::EditAllergens) {
        String::new()
    } else {
        q.to_owned()
    };
    Suggestions {
        matches: ingredient_hits
            .chain(sub_hits)
            .take(MAX_SUGGESTIONS)
            .collect(),
        new_label: t_with(state, "recipe-add-new", &[("name", new_name.clone())]),
        new_name,
        new_open: add.new,
        roles: IngredientRole::ALL
            .into_iter()
            .map(|role| SelectOption {
                value: role.code().to_owned(),
                label: t(state, role_label(role)),
                selected: role.code() == add.role,
            })
            .collect(),
        role_error: add.role_error.clone(),
        allergens: Allergen::ALL
            .into_iter()
            .map(|allergen| AllergenBox {
                field: format!("allergen_{}", allergen.code()),
                code: allergen.code(),
                checked: add.allergens.contains(&allergen),
            })
            .collect(),
    }
}

#[derive(Template)]
#[template(path = "staff/recipe_version.html")]
struct VersionPage<'a> {
    layout: Layout<'a>,
    heading: String,
    back: String,
    saved: bool,
    frozen: bool,
    status: String,
    freeze: String,
    current: Option<String>,
    new_version: String,
    facts: Table,
    editor: Editor,
    notes: String,
    method: MethodView,
}

fn facts(state: &AppState, loaded: &Loaded) -> Table {
    let version = &loaded.version;
    let none = || t(state, "recipe-none");
    let analysis = loaded.formula.analysis().ok();
    Table {
        id: "kenmerken".to_owned(),
        caption: t_with(
            state,
            "recipe-version-number",
            &[("n", version.version.to_string())],
        ),
        head: vec![
            Cell::text(t(state, "recipe-status")),
            Cell::text(status_text(state, version)),
        ],
        rows: vec![
            vec![
                Cell::text(t(state, "recipe-prefermented-flour")),
                Cell::text(
                    analysis
                        .and_then(|a| a.prefermented_flour)
                        .map_or_else(none, pct_text),
                ),
            ],
            vec![
                Cell::text(t(state, "recipe-lead-time")),
                Cell::text(version.lead_time_hours.map_or_else(none, |hours| {
                    t_with(
                        state,
                        "recipe-lead-time-value",
                        &[("hours", hours.to_string())],
                    )
                })),
            ],
        ],
    }
}

pub async fn version_page(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path((id, version)): Path<(String, String)>,
    Query(fields): Query<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let loaded = load(&state, &id, &version).await?;
        let scale = Scale::read(&state, &fields, &loaded.products);
        let (recipe, version) = (&loaded.recipe, &loaded.version);
        let path = version_path(recipe.id, version.id);
        let status = match version.frozen_at {
            Some(at) => t_with(&state, "recipe-frozen-info", &[("date", date_text(at))]),
            None => t(&state, "recipe-draft-info"),
        };
        let editor = editor(
            &state,
            &loaded,
            EditorInput {
                scale: &scale,
                page: path.clone(),
                switch: (!version.is_frozen())
                    .then(|| (format!("{path}/edit"), t(&state, "recipe-edit"))),
                notice: None,
                saved: false,
                add: None,
            },
        );
        let now = state.clock.now_in_bakery();
        let method = load_view(
            &state,
            version.id,
            &fields,
            &Shown {
                page: path.clone(),
                form: "schaal",
                pieces: !scale.pieces.is_empty() && scale.pieces_error.is_empty(),
                base: &default_start(&now),
            },
        )
        .await?;
        Ok(pages::render(
            &state,
            StatusCode::OK,
            &VersionPage {
                layout: layout(&state, &session).await?,
                heading: t_with(
                    &state,
                    "recipe-version-title",
                    &[
                        ("name", recipe.name.clone()),
                        ("n", version.version.to_string()),
                    ],
                ),
                back: recipe_path(recipe.id),
                saved: fields.checked("opgeslagen"),
                frozen: version.is_frozen(),
                status,
                freeze: format!("{path}/freeze"),
                current: (recipe.current_version_id != Some(version.id))
                    .then(|| format!("{path}/current")),
                new_version: format!("{}/versions", recipe_path(recipe.id)),
                facts: facts(&state, &loaded),
                editor,
                notes: version.notes.clone(),
                method,
            },
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

#[derive(Template)]
#[template(path = "staff/recipe_edit.html")]
struct EditPage<'a> {
    layout: Layout<'a>,
    heading: String,
    saved: bool,
    editor: Editor,
    details_action: String,
    lead_time: String,
    lead_time_error: String,
    notes: String,
    method: MethodEditor,
}

/// The edit page after a change, or with what went wrong.
pub(super) struct EditView {
    scale: Scale,
    notice: Option<Message>,
    /// A line change was saved; shown inside the table, which htmx swaps.
    line_saved: bool,
    /// The details were saved; shown at the top of the page.
    details_saved: bool,
    add: AddState,
    lead_time: Option<String>,
    lead_time_error: String,
    notes: Option<String>,
    pub(super) method: MethodState,
}

impl EditView {
    fn new(scale: Scale) -> Self {
        Self {
            scale,
            notice: None,
            line_saved: false,
            details_saved: false,
            add: AddState::empty(),
            lead_time: None,
            lead_time_error: String::new(),
            notes: None,
            method: MethodState::default(),
        }
    }

    /// The page at the scale in `fields`, as it is stored.
    pub(super) fn read(state: &AppState, fields: &Fields, loaded: &Loaded) -> Self {
        Self::new(Scale::read(state, fields, &loaded.products))
    }

    fn refused(scale: Scale, text: String, add: AddState) -> Self {
        Self {
            notice: Some(Message {
                kind: "error",
                text,
            }),
            add,
            ..Self::new(scale)
        }
    }
}

/// Who is asking and with which status the page goes back.
pub(super) struct Reply<'a> {
    pub(super) state: &'a AppState,
    pub(super) session: &'a Session,
    pub(super) staff: &'a Staff,
    pub(super) status: StatusCode,
}

pub(super) async fn render_edit(reply: &Reply<'_>, loaded: &Loaded, view: EditView) -> Outcome {
    let state = reply.state;
    let path = version_path(loaded.recipe.id, loaded.version.id);
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    let all = ingredients::list(&mut conn)
        .await
        .map_err(|e| fail(state, &e))?;
    let add = (!loaded.version.is_frozen()).then(|| AddRow {
        action: format!("{path}/lines"),
        search: format!("{path}/search"),
        q: view.add.q.clone(),
        pct: view.add.pct.clone(),
        gram: view.add.gram.clone(),
        suggestions: suggestions(state, reply.staff, &all, &loaded.subs, &view.add),
    });
    let stored = recipes::method::method(&mut conn, loaded.version.id)
        .await
        .map_err(|e| fail(state, &e))?;
    let method = method_editor(
        state,
        &path,
        view.scale.grams.to_string(),
        &stored,
        view.method,
    );
    let editor = editor(
        state,
        loaded,
        EditorInput {
            scale: &view.scale,
            page: format!("{path}/edit"),
            switch: Some((path.clone(), t(state, "recipe-done"))),
            notice: view.notice,
            saved: view.line_saved,
            add,
        },
    );
    Ok(pages::render(
        state,
        reply.status,
        &EditPage {
            layout: layout(state, reply.session).await?,
            heading: t_with(
                state,
                "recipe-edit-title",
                &[
                    ("name", loaded.recipe.name.clone()),
                    ("n", loaded.version.version.to_string()),
                ],
            ),
            saved: view.details_saved,
            editor,
            details_action: format!("{path}/edit"),
            lead_time: view.lead_time.unwrap_or_else(|| {
                loaded
                    .version
                    .lead_time_hours
                    .map(|h| h.to_string())
                    .unwrap_or_default()
            }),
            lead_time_error: view.lead_time_error,
            notes: view.notes.unwrap_or_else(|| loaded.version.notes.clone()),
            method,
        },
    ))
}

pub async fn edit_page(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path((id, version)): Path<(String, String)>,
    Query(fields): Query<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let loaded = load(&state, &id, &version).await?;
        if loaded.version.is_frozen() {
            let path = version_path(loaded.recipe.id, loaded.version.id);
            return Ok(Redirect::to(&path).into_response());
        }
        let mut view = EditView::new(Scale::read(&state, &fields, &loaded.products));
        view.details_saved = fields.checked("opgeslagen");
        let reply = Reply {
            state: &state,
            session: &session,
            staff: &staff,
            status: StatusCode::OK,
        };
        render_edit(&reply, &loaded, view).await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// A refused change goes back with 422 (409 for a frozen version); htmx gets 200, because
/// it only swaps 2xx.
pub(super) fn refused_status(headers: &HeaderMap, error: Option<&recipes::Error>) -> StatusCode {
    if matches!(error, Some(recipes::Error::Frozen)) && !is_htmx(headers) {
        StatusCode::CONFLICT
    } else {
        invalid_status(headers)
    }
}

fn error_text(state: &AppState, error: &recipes::Error) -> Option<String> {
    let id = match error {
        recipes::Error::Frozen => "recipe-error-frozen",
        recipes::Error::Stale => "recipe-error-stale",
        recipes::Error::Invalid("line") => "recipe-error-line-gone",
        recipes::Error::Invalid("lines") => "recipe-error-lines",
        recipes::Error::Invalid("pct") => "recipe-line-error-pct",
        recipes::Error::Invalid("lead_time_hours") => "recipe-lead-time-error",
        recipes::Error::Invalid("name") => "ingredient-error-name",
        recipes::Error::Nesting => "recipe-error-nesting",
        recipes::Error::Duplicate => "ingredient-error-duplicate",
        _ => return None,
    };
    Some(t(state, id))
}

pub async fn save_details(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path((id, version)): Path<(String, String)>,
    Form(fields): Form<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let loaded = load(&state, &id, &version).await?;
        let lead_time = fields.text("lead_time").to_owned();
        let notes = fields.raw("notes").trim().replace("\r\n", "\n");
        let hours = if lead_time.is_empty() {
            Some(None)
        } else {
            lead_time
                .parse::<i32>()
                .ok()
                .filter(|h| (0..=MAX_LEAD_TIME_HOURS).contains(h))
                .map(Some)
        };
        let result = match hours {
            Some(hours) => {
                let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
                store::save_details(&mut conn, loaded.version.id, hours, &notes).await
            }
            None => Err(recipes::Error::Invalid("lead_time_hours")),
        };
        let error = match result {
            Ok(_) => {
                let path = version_path(loaded.recipe.id, loaded.version.id);
                let scale = Scale::read(&state, &fields, &loaded.products);
                return Ok(saved(&headers, &format!("{path}/edit?{}", scale.query())));
            }
            Err(error) => error,
        };
        let text = error_text(&state, &error).ok_or_else(|| fail(&state, &error))?;
        let mut view = EditView::new(Scale::read(&state, &fields, &loaded.products));
        if matches!(error, recipes::Error::Invalid("lead_time_hours")) {
            view.lead_time_error = text;
        } else {
            view.notice = Some(Message {
                kind: "error",
                text,
            });
        }
        view.lead_time = Some(lead_time);
        view.notes = Some(notes);
        let reply = Reply {
            state: &state,
            session: &session,
            staff: &staff,
            status: refused_status(&headers, Some(&error)),
        };
        render_edit(&reply, &loaded, view).await
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

/// A typed amount: a percentage, or grams that become one at the current scale.
fn amount(state: &AppState, fields: &Fields, dough: u64, line_sum: u64) -> Result<Pct, String> {
    let pct = fields.text("pct");
    let gram = fields.text("gram");
    let by_grams = !gram.is_empty() || (fields.checked("gram") && !fields.checked("pct"));
    if pct.is_empty() && by_grams {
        if line_sum == 0 {
            return Err(t(state, "recipe-add-error-first-pct"));
        }
        return gram
            .parse::<u64>()
            .ok()
            .and_then(|g| pct_for_grams(g, dough, line_sum))
            .filter(|p| p.thousandths() > 0 && *p <= MAX_LINE_PCT)
            .ok_or_else(|| t(state, "recipe-line-error-grams"));
    }
    parse_pct(pct)
        .filter(|p| p.thousandths() > 0 && *p <= MAX_LINE_PCT)
        .ok_or_else(|| t(state, "recipe-line-error-pct"))
}

#[derive(Clone, Copy)]
enum Change {
    Set(usize),
    Add,
    Remove(usize),
}

/// A posted line change with what it needs to become a [`LineEdit`].
struct Request<'a> {
    state: &'a AppState,
    staff: &'a Staff,
    fields: &'a Fields,
    loaded: &'a Loaded,
    ingredients: &'a [Ingredient],
    scale: &'a Scale,
}

/// The edit to make, and for a new ingredient, what to create first; or why not.
type Planned = Result<(LineEdit, Option<IngredientInput>), String>;

impl Request<'_> {
    fn line_at(&self, position: usize) -> Option<LinePart> {
        let posted = parse_part(self.fields.text("part"))?;
        let line = self.loaded.formula.lines.get(position)?;
        (line_part(line) == posted).then_some(posted)
    }

    fn amount(&self) -> Result<Pct, String> {
        amount(
            self.state,
            self.fields,
            self.scale.grams,
            self.loaded.formula.line_sum(),
        )
    }

    fn gone(&self) -> String {
        t(self.state, "recipe-error-line-gone")
    }

    /// An ingredient or sub-recipe whose name is exactly what was typed.
    fn named(&self, q: &str) -> Option<LinePart> {
        if q.is_empty() {
            return None;
        }
        self.ingredients
            .iter()
            .find(|i| same_name(&i.name, q))
            .map(|i| LinePart::Ingredient(i.id))
            .or_else(|| {
                self.loaded
                    .subs
                    .iter()
                    .find(|s| same_name(&s.recipe_name, q))
                    .map(|s| LinePart::Sub(s.version_id))
            })
    }

    fn plan(&self, change: Change, add: &mut AddState) -> Step<Planned> {
        Ok(match change {
            Change::Set(position) => match self.line_at(position) {
                Some(part) => self.amount().map(|pct| {
                    (
                        LineEdit::SetPct {
                            position,
                            part,
                            pct,
                        },
                        None,
                    )
                }),
                None => Err(self.gone()),
            },
            Change::Remove(position) => match self.line_at(position) {
                Some(part) => Ok((LineEdit::Remove { position, part }, None)),
                None => Err(self.gone()),
            },
            Change::Add if add.new => {
                require(self.state, self.staff, Permission::EditAllergens)?;
                self.plan_new(add)
            }
            Change::Add => {
                let part = parse_part(self.fields.text("part")).or_else(|| self.named(&add.q));
                match part {
                    Some(part) => self
                        .amount()
                        .map(|pct| (LineEdit::Add(LineInput { pct, part }), None)),
                    None => Err(t(self.state, "recipe-add-error-choose")),
                }
            }
        })
    }

    fn plan_new(&self, add: &mut AddState) -> Planned {
        if add.q.is_empty() {
            return Err(t(self.state, "ingredient-error-name"));
        }
        let pct = self.amount()?;
        let Ok(role) = add.role.parse::<IngredientRole>() else {
            add.role_error = t(self.state, "ingredient-error-role");
            return Err(add.role_error.clone());
        };
        let input = IngredientInput {
            name: add.q.clone(),
            label_name: String::new(),
            role,
            supplier_name: String::new(),
            allergens: add.allergens.clone(),
        };
        let placeholder = LinePart::Ingredient(IngredientId::new(0));
        Ok((
            LineEdit::Add(LineInput {
                pct,
                part: placeholder,
            }),
            Some(input),
        ))
    }
}

/// Makes the edit; a new ingredient is created and added in one transaction.
async fn apply(
    state: &AppState,
    version: RecipeVersionId,
    seen: &str,
    edit: LineEdit,
    new: Option<IngredientInput>,
) -> Step<Result<LineEdit, recipes::Error>> {
    let mut conn = state.pool.acquire().await.map_err(|e| fail(state, &e))?;
    Ok(match (edit, new) {
        (LineEdit::Add(line), Some(input)) => {
            store::add_new_ingredient(&mut conn, version, seen, &input, line.pct)
                .await
                .map(|created| {
                    LineEdit::Add(LineInput {
                        part: LinePart::Ingredient(created.id),
                        ..line
                    })
                })
        }
        (edit, _) => store::edit_line(&mut conn, version, seen, &edit)
            .await
            .map(|_| edit),
    })
}

/// The scale after the edit: the same grams per percent, and the open sub-recipe lines
/// following a removal.
fn scale_after(formula: &Formula, scale: &Scale, edit: &LineEdit) -> Scale {
    let sum = formula.line_sum();
    let old = |position: usize| {
        formula
            .lines
            .get(position)
            .map_or(0, |line| u64::from(line.pct.thousandths()))
    };
    let (new_sum, open) = match *edit {
        LineEdit::SetPct { position, pct, .. } => (
            sum - old(position) + u64::from(pct.thousandths()),
            scale.open.clone(),
        ),
        LineEdit::Add(line) => (sum + u64::from(line.pct.thousandths()), scale.open.clone()),
        LineEdit::Remove { position, .. } => (
            sum - old(position),
            scale
                .open
                .iter()
                .filter(|i| **i != position)
                .map(|i| if *i > position { i - 1 } else { *i })
                .collect(),
        ),
    };
    let grams = match dough_for_sum(scale.grams, sum, new_sum) {
        0 => DEFAULT_WEIGHT_G,
        grams => grams.min(MAX_WEIGHT_G),
    };
    Scale::weight(grams, open)
}

/// The route parameters and form of a line change.
struct Posted {
    id: String,
    version: String,
    fields: Fields,
    change: Change,
}

async fn change_line(
    state: AppState,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    posted: Posted,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let Posted {
            id,
            version,
            fields,
            change,
        } = posted;
        let loaded = load(&state, &id, &version).await?;
        let scale = Scale::read(&state, &fields, &loaded.products);
        let mut add = match change {
            Change::Add => AddState::posted(&fields),
            Change::Set(_) | Change::Remove(_) => AddState::empty(),
        };
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let all = ingredients::list(&mut conn)
            .await
            .map_err(|e| fail(&state, &e))?;
        drop(conn);
        let planned = if loaded.version.is_frozen() {
            None
        } else {
            let request = Request {
                state: &state,
                staff: &staff,
                fields: &fields,
                loaded: &loaded,
                ingredients: &all,
                scale: &scale,
            };
            Some(request.plan(change, &mut add)?)
        };
        let outcome = match planned {
            None => Err(recipes::Error::Frozen),
            Some(Ok((edit, new))) => {
                apply(&state, loaded.version.id, fields.text("seen"), edit, new).await?
            }
            Some(Err(text)) => {
                let reply = Reply {
                    state: &state,
                    session: &session,
                    staff: &staff,
                    status: refused_status(&headers, None),
                };
                return render_edit(&reply, &loaded, EditView::refused(scale, text, add)).await;
            }
        };
        let edit = match outcome {
            Ok(edit) => edit,
            Err(recipes::Error::NotFound) => return Err(not_found(&state).into()),
            Err(error) => {
                let text = error_text(&state, &error).ok_or_else(|| fail(&state, &error))?;
                let reply = Reply {
                    state: &state,
                    session: &session,
                    staff: &staff,
                    status: refused_status(&headers, Some(&error)),
                };
                let current = if matches!(error, recipes::Error::Stale) {
                    load(&state, &id, &version).await?
                } else {
                    loaded
                };
                return render_edit(&reply, &current, EditView::refused(scale, text, add)).await;
            }
        };
        let after = scale_after(&loaded.formula, &scale, &edit);
        let path = version_path(loaded.recipe.id, loaded.version.id);
        let url = format!("{path}/edit?{}", after.query());
        if !is_htmx(&headers) {
            return Ok(Redirect::to(&format!("{url}&opgeslagen=1")).into_response());
        }
        let reloaded = load(&state, &id, &version).await?;
        let mut view = EditView::new(after);
        view.line_saved = true;
        let reply = Reply {
            state: &state,
            session: &session,
            staff: &staff,
            status: StatusCode::OK,
        };
        let mut page = render_edit(&reply, &reloaded, view).await?;
        if let Ok(value) = HeaderValue::try_from(url) {
            page.headers_mut()
                .insert(HeaderName::from_static("hx-replace-url"), value);
        }
        Ok(page)
    };
    run.await.unwrap_or_else(Rejection::into_response)
}

pub async fn set_line(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path((id, version, position)): Path<(String, String, String)>,
    Form(fields): Form<Fields>,
) -> Response {
    let Ok(position) = position.parse() else {
        return not_found(&state);
    };
    let posted = Posted {
        id,
        version,
        fields,
        change: Change::Set(position),
    };
    change_line(state, staff, session, headers, posted).await
}

pub async fn remove_line(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path((id, version, position)): Path<(String, String, String)>,
    Form(fields): Form<Fields>,
) -> Response {
    let Ok(position) = position.parse() else {
        return not_found(&state);
    };
    let posted = Posted {
        id,
        version,
        fields,
        change: Change::Remove(position),
    };
    change_line(state, staff, session, headers, posted).await
}

pub async fn add_line(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    headers: HeaderMap,
    Path((id, version)): Path<(String, String)>,
    Form(fields): Form<Fields>,
) -> Response {
    let posted = Posted {
        id,
        version,
        fields,
        change: Change::Add,
    };
    change_line(state, staff, session, headers, posted).await
}

#[derive(Template)]
#[template(path = "staff/recipe_search.html")]
struct SearchFragment<'a> {
    layout: Layout<'a>,
    suggestions: Suggestions,
}

pub async fn search(
    State(state): State<AppState>,
    staff: Staff,
    session: Session,
    Path((id, version)): Path<(String, String)>,
    Query(fields): Query<Fields>,
) -> Response {
    let run = async {
        require(&state, &staff, Permission::Production)?;
        let (recipe, _) = load_version(&state, &id, &version).await?;
        let mut conn = state.pool.acquire().await.map_err(|e| fail(&state, &e))?;
        let all = ingredients::list(&mut conn)
            .await
            .map_err(|e| fail(&state, &e))?;
        let subs = store::sub_choices(&mut conn, recipe.id)
            .await
            .map_err(|e| fail(&state, &e))?;
        let add = AddState::typed(fields.text("q").to_owned());
        Ok(pages::render(
            &state,
            StatusCode::OK,
            &SearchFragment {
                layout: layout(&state, &session).await?,
                suggestions: suggestions(&state, &staff, &all, &subs, &add),
            },
        ))
    };
    run.await.unwrap_or_else(Rejection::into_response)
}
