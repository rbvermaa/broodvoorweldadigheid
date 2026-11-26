# 0053 Compact Bread Drop list and drop facts

Status: accepted, 2026-11-26

Owner feedback (F1, F1a, F4c): the Bread Drop cards on /drops were too wordy, "Normale
verkoop" is implied, and the drop page looked cramped. The shop now lists each drop as one
short row, the drop page shows its facts as a short definition list, and the home page shows
the drops too.

| Choice | Why |
|---|---|
| Row: date (`za 12 dec`, with the year only when it is not this year) linking to the drop, first pickup (`11:00–12:00 De Proef`, plus `+1 ander moment` for the rest), order window, `voor <goed doel>` when there is one, status pill | Every fact fits on two phone lines. The address and the other pickup moments are on the drop page |
| A pill only for Open, Uitverkocht, Gesloten and Gaat niet door | These tell the customer to act or not to bother. A planned drop shows `bestellen vanaf <opens_at>` when ordering opens later, else `bestel-deadline …`, which does not read as open; "Gepland" and "Menu bekend" said nothing the row does not |
| No order window once ordering is over (sold out, closed, cancelled) | The pill says it; a past deadline is noise |
| `PublicDrop` carries `opens_at` | The calendar needs it for "bestellen vanaf"; the status rule (ADR 0008) does not change |
| Drop page: `dl` with Ophalen (time, plus the date when it is not the drop's date; location, address below in small text), Bestellen tot or Bestellen vanaf, Goed doel (name, linked, and its description) | Calm, scannable; the charity's description is what F4c asks for on the drop page |
| Home: the earliest open drop as a card with a Bestellen button, the other coming drops as the list. Without an open drop, only the list | The owner's rule. If the drops cannot be read, the home page shows without them instead of failing |
| On a phone each row wraps in two short lines (date, pickup and pill; then order window and charity); from md the rows share columns through `subgrid` | Aligned columns on desktop, no sideways scroll on a phone. The table component scrolls sideways, so it does not fit here |
| `.split` is a grid (`minmax(0, auto)` and the aside width) instead of flex | The owner saw the cart cut off at the right edge. We could not reproduce it in Firefox; a grid track cannot push the aside past the container |
| Own icons go in `[own]` in `design/icons.toml`, checked like Lucide's shapes | Lucide dropped brand icons. The Instagram glyph is drawn inline, so nothing loads from Instagram or Meta |
