# 0049 Recipe instructions: free text or steps with a schedule

Status: proposed, 2026-11-25

Owner request (F14): instructions on a recipe like the baking app, with steps such as
"09:00 Autolyse 30 min" from a start time, optional, and a way to write plain text instead.

| Choice | Why |
|---|---|
| Per version, the instructions are none, a free text or steps (`recipe_version.method_kind`, `method_text`, table `recipe_step`). Steps exist only when the kind is steps, and the text is empty unless the kind is text (check and triggers) | One kind at a time, so the page never has to choose between two contents |
| Switching kind clears the old content in the same transaction. The page asks first (confirm dialog) when there is content; with nothing to lose it switches at once | Converting text to steps cannot be done well (one line is not one step). It is a draft, and the frozen version before it still has the old content |
| A step has a name (1–80 characters), and optionally a duration in whole minutes (1–10080), a temperature in whole °C (−30 to 300) and notes (up to 1000 characters). At most 40 steps; text up to 10 000 characters | One number for the duration keeps the schedule exact. A range can go in the notes |
| Text and notes are plain text: shown escaped, a blank line starts a paragraph, a line end becomes a line break. No Markdown or HTML | Nothing to sanitise, and the baker writes as in a notebook |
| The instructions belong to the version: only a draft changes (`Error::Frozen`, and the frozen trigger of ADR 0037 also covers `recipe_step`); a new draft ("Nieuwe versie bewerken"), saving a frozen version and promoting a batch (F14b) copy them | Same rules as lines. A promoted version takes the instructions of the version the batch was made from |
| Every change posts the stamp of the instructions the page showed (`method::stamp`, FNV-1a), as line edits do; an out-of-date page changes nothing. The free text is saved whole, without a stamp, like the notes | Two tabs or a slow phone never move or remove the wrong step |
| Instructions touch no allergen, label or costing data and no trigger of ADR 0037 | They change nothing in the product |
| Schedule (`recipes::schedule`): each step starts at the start time plus the durations of the steps before it; the end is the start plus all durations. A step without duration shows its start time and adds nothing; the end says how many steps have no duration | The baker sees every step's time. A warning at the end is enough to see that the end is too early |
| Durations are elapsed time (jiff timestamp arithmetic in Europe/Amsterdam): on the day the clocks change a 60-minute step from 01:30 ends at 03:30 in spring and at 02:30 (the second time) in autumn | Dough does not care about the clock |
| A step on a later date than the start shows "+1 dag" (or more) next to its time | A cold retard overnight stays readable |
| The start time is `start=HH:MM` in the URL, on the date of today, rounded to the nearest five minutes when absent; nothing is stored. On the recipe page the field belongs to the scale form; a change through htmx merges the start into the URL the page shows now, so a scale changed live is kept | Like the scale setting (ADR 0037) |
| The batch page shows the instructions of the batch's version, read-only, with the same schedule; the start is when the batch was mixed, or now when it is not mixed yet | The batch's version is frozen, so it needs no copy of its own |
| The instructions sit under the recipe table, in the wide column of the split layout | One column on a phone |
| On a draft the steps are a compact table like the recipe table: time, name, minutes and °C per row, with notes and the move and remove buttons on a second line. Each cell belongs to its row's form; Enter or leaving a changed cell saves the row through htmx, and htmx keeps the focus on the cell by its id. A new step is one row with name and duration; temperature and notes go in the table after | Owner: compact and easy, like the ingredient table. Saving the whole row keeps one endpoint and one validation |

Decided 2026-11-25: no ranges for durations and no steps fixed to a clock time; the start
defaults to now, not to the bake day or pickup time.
