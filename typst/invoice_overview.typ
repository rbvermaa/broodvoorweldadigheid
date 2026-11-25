#let data = json("/data.json")
#let l = data.labels

#set document(title: data.title, date: none)
#set page(paper: "a4", flipped: true, margin: 15mm)
#set text(font: "Inter", size: 10pt, lang: "nl", fallback: false)
#show heading: set text(font: "Oswald", weight: "regular")
#show table: set text(size: 9pt)

= #data.title
#data.period

#grid(
  columns: (1fr, 1fr),
  column-gutter: 8mm,
  stack(spacing: 4pt, strong(data.seller.heading), ..data.seller.lines),
  stack(spacing: 4pt, strong(data.customer.heading), ..data.customer.lines),
)

#data.intro

#if data.lines.len() == 0 [
  #l.empty
] else [
  == #l.orders
  #table(
    columns: (auto, auto, 1fr) + (auto,) * 6,
    align: (left, left, left) + (right,) * 6,
    stroke: 0.5pt,
    table.header(..l.columns.map(strong)),
    ..data.lines.flatten(),
  )

  == #l.totals
  #table(
    columns: (1fr,) + (auto,) * 3,
    align: (left,) + (right,) * 3,
    stroke: 0.5pt,
    table.header(..l.total_columns.map(strong)),
    ..data.totals.flatten(),
    table.footer(..data.total.map(strong)),
  )
]
