#let data = json("/data.json")
#let l = data.labels

#set document(title: data.title + " " + data.date, date: none)
#set page(paper: "a4", margin: 15mm)
#set text(font: "Inter", size: 11pt, lang: "nl", fallback: false)
#show heading: set text(font: "Oswald", weight: "regular")

= #data.title
#data.date

#data.basis

#if data.ingredients.len() == 0 and data.missing.len() == 0 [
  #l.empty
] else if data.ingredients.len() > 0 {
  table(
    columns: (1fr, auto),
    align: (left, right),
    stroke: 0.5pt,
    table.header(strong(l.ingredient), strong(l.amount)),
    ..data.ingredients.map(i => (i.name, i.amount)).flatten(),
    table.footer(strong(l.sum), strong(data.sum)),
  )
}

#if data.doughs.len() > 0 {
  heading(level: 2, l.doughs)
  table(
    columns: (auto, 1fr, auto),
    align: (left, left, right),
    stroke: 0.5pt,
    table.header(strong(l.recipe), strong(l.products), strong(l.amount)),
    ..data.doughs.map(d => (d.name, d.products, d.amount)).flatten(),
  )
}

#if data.missing.len() > 0 {
  heading(level: 2, l.missing)
  par(l.missing_intro)
  table(
    columns: (1fr, auto, auto),
    align: (left, right, left),
    stroke: 0.5pt,
    table.header(strong(l.product), strong(l.pieces), strong(l.reason)),
    ..data.missing.map(m => (m.name, m.pieces, m.reason)).flatten(),
  )
}
