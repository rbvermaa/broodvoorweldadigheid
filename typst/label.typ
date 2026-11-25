#let data = json("/data.json")

#set document(date: none)
#set page(width: data.width_mm * 1mm, height: auto, margin: 1.5mm)
#set text(font: "Inter", size: 6pt, lang: "nl", fallback: false)
#set par(leading: 0.3em, spacing: 0.4em, justify: false)

#let front = {
  grid(
    columns: (1fr, 12mm),
    column-gutter: 1mm,
    {
      block(below: 0.5em, text(font: "Oswald", weight: "bold", size: 9pt, upper(data.product)))
      for row in data.fields {
        par[#strong(row.label) #row.value]
      }
    },
    image("/logo.png", width: 12mm),
  )
  par(data.storage)
  par(data.business)
}

#let back = {
  par[#strong(data.ingredients_label) #data.ingredients.map(i => if i.bold { strong(i.name) } else { i.name }).join(", ").]
  if data.may_contain != none {
    par(data.may_contain)
  }
}

#if data.part == "front" {
  front
} else if data.part == "back" {
  back
} else {
  front
  back
}
