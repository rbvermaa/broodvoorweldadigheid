Askama HTML templates. Pages extend `base.html`, build their UI only from the macros in
`components/`, and get their text from Fluent through `layout.t("message-id")`; never
write Dutch text, `style` attributes or new classes here. Every component and state is
shown on `/gallery` (with `GALLERY=true`).
