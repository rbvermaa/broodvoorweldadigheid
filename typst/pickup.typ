#let data = json("/data.json")
#let l = data.labels

#set document(title: data.title + " " + data.date, date: none)
#set page(paper: "a4", margin: 15mm)
#set text(font: "Inter", size: 11pt, lang: "nl", fallback: false)
#show heading: set text(font: "Oswald", weight: "regular")

#let tick(done) = box(
  width: 5mm,
  height: 5mm,
  stroke: 0.75pt,
  fill: if done { black } else { none },
)

= #data.title
#data.date

#for slot in data.slots [
  == #slot.heading
  #if slot.orders.len() == 0 [
    #l.empty
  ] else {
    table(
      columns: (auto, auto, 1fr, 1.4fr, auto),
      align: (center + horizon, left, left, left, left),
      stroke: 0.5pt,
      table.header(
        strong(l.done),
        strong(l.number),
        strong(l.customer),
        strong(l.items),
        strong(l.status),
      ),
      ..slot
        .orders
        .map(o => (
          tick(o.picked_up),
          o.number,
          {
            let name = if o.erased { emph(o.name) } else { strong(o.name) }
            if o.phone == "" { name } else { stack(spacing: 4pt, name, o.phone) }
          },
          stack(spacing: 4pt, ..o.lines),
          o.status,
        ))
        .flatten(),
    )
  }
]
