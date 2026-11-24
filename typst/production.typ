#let data = json("/data.json")
#let l = data.labels

#set document(title: data.title + " " + data.date, date: none)
#set page(paper: "a4", margin: 15mm)
#set text(font: "Inter", size: 11pt, lang: "nl", fallback: false)
#show heading: set text(font: "Oswald", weight: "regular")

= #data.title
#data.date

#if data.products.len() == 0 [
  #l.empty
] else {
  let n = l.columns.len()
  table(
    columns: (1fr,) + (auto,) * n,
    align: (left,) + (right,) * n,
    stroke: 0.5pt,
    table.header(strong(l.product), ..l.columns.map(strong)),
    ..data
      .products
      .map(p => (p.name, ..p.cells.slice(0, n - 1), strong(p.cells.last())))
      .flatten(),
    table.footer(strong(l.sum), ..data.sum.map(strong)),
  )
}

#if data.pools.len() > 0 [
  == #l.pools
  #table(
    columns: (1fr, auto, auto),
    align: (left, right, right),
    stroke: 0.5pt,
    table.header(strong(l.pool), strong(l.used), strong(l.max)),
    ..data.pools.map(p => (p.name, p.used, p.max)).flatten(),
  )
]
