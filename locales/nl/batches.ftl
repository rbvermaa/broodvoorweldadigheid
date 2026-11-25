# Back office: batches per bake day (F14a)
batches-title = Batches
# $date: zaterdag 5 december 2026
batches-heading = Batches voor { $date }
batches-back = Terug naar de bakdag
batches-capacity-missing = De mengcapaciteit is nog niet ingevuld. Daarom maakt het systeem bij de bestel-deadline geen batches. Je kunt wel een batch met de hand starten.
# $kg is preformatted: 12,5
batches-capacity-current = Bij de bestel-deadline verdeelt het systeem elk deeg over batches van hooguit { $kg } kg.
batches-capacity = Mengcapaciteit in kg deeg per batch
batches-capacity-hint = Bijvoorbeeld 12,5. Leeg laten zet het maken van batches uit.
batches-capacity-error = Vul een gewicht in kg in, bijvoorbeeld 12,5.
batches-split = Maak batches
batches-split-hint = Verdeelt de stuks die nog niet in een batch zitten: bestellingen en extra productie.
batches-split-done = { $n ->
    [0] Er waren geen nieuwe stukken om in batches te verdelen.
    [one] 1 batch gemaakt.
   *[other] { $n } batches gemaakt.
}
batches-skip-no-recipe = { $product } heeft geen recept, dus geen batch.
batches-skip-no-version = Het recept van { $product } heeft geen versie in gebruik, dus geen batch.
batches-skip-no-dough = { $product } heeft geen deeggewicht per stuk, dus geen batch.
batches-skip-empty = Het recept van { $product } heeft nog geen regels, dus geen batch.
batches-skip-too-heavy = Eén stuk { $product } heeft meer deeg dan de mixer aankan. Start een batch met de hand.
batches-caption = Batches van deze bakdag
batches-empty-title = Nog geen batches
batches-empty-text = Maak batches of start er een met de hand.
batches-manual-title = Batch met de hand starten
batches-manual-product = Product
batches-manual-pieces = Aantal stuks
batches-manual-start = Batch starten
batches-manual-error-product = Kies een product met een recept.
batches-manual-error-pieces = Vul een aantal stuks in van 1 tot 100.000.
batches-manual-error-recipe = Dit product heeft geen recept met regels in gebruik, of geen deeggewicht per stuk.

batch-code = Code
batch-recipe = Recept
batch-pieces = Stuks
batch-dough = Deeg
batch-status = Status
batch-status-planned = Gepland
batch-status-adjusted = Aangepast
batch-status-mixed = Gemengd
batch-status-baked = Gebakken
# $name: Desembrood, $n: 3
batch-recipe-version = { $name } versie { $n }
# $kg is preformatted: 12,5
batch-kg = { $kg } kg
batch-origin-cutoff = Gemaakt bij de bestel-deadline
batch-origin-manual = Met de hand gestart
# $code: 261205-2
batch-heading = Batch { $code }
batch-back = Alle batches van deze bakdag
batch-facts = Over deze batch
batch-origin = Herkomst
# $grams is preformatted: 9.000
batch-lines-caption = Regels voor { $grams } g deeg
batch-line = Ingrediënt
batch-line-base = Recept
batch-line-actual = Deze batch
batch-line-grams = Gram
# $name: Water
batch-line-changed = { $name } (aangepast)
batch-line-added = { $name } (toegevoegd)
batch-line-none = —
# $name: Desem, $n: 2
batch-sub = { $name } versie { $n }
batch-totals-caption = Per ingrediënt
batch-no-lines = Alle regels staan op 0%. Er valt niets af te wegen.
batch-adjust-title = Aanpassen
batch-adjust-hint = Pas percentages en stuks aan; de grammen rekenen meteen mee. 0% laat een regel weg.
# $name: Water
batch-line-pct = { $name } in %
# $product: Desem wit
batch-output-pieces = { $product }: stuks
batch-reason = Reden
batch-reason-hint = Niet verplicht. Bijvoorbeeld: deze bloem neemt minder water op.
batch-error-pct = Vul een percentage in van 0 tot 1000, bijvoorbeeld 72,5.
batch-error-pieces = Vul een aantal stuks in van 0 tot 100.000.
batch-error-reason = Houd de reden korter dan 200 tekens.
batch-add-title = Ingrediënt toevoegen
batch-add-ingredient = Ingrediënt
batch-add-pct = Percentage
batch-add = Toevoegen
batch-add-error-ingredient = Kies een ingrediënt.
batch-add-error-pct = Vul een percentage in van meer dan 0 tot 1000.
batch-add-error-full = Deze batch heeft al het maximale aantal regels.
batch-mix = Gemengd
# $code: 261205-2
batch-mix-text = Na het mengen liggen de regels en stuks van batch { $code } vast. Je kunt ze daarna niet meer aanpassen.
batch-mix-confirm = Ja, gemengd
batch-locked = Deze batch is gemengd. De regels en stuks liggen vast.
batch-actuals-title = Na het bakken
batch-actuals-hint = Vul per product in hoeveel stuks uit de oven kwamen en hoeveel daarvan afval zijn.
# $product: Desem wit
batch-actual-pieces = { $product }: gebakken stuks
batch-actual-waste = { $product }: waarvan afval
batch-error-waste = Afval kan niet meer zijn dan de gebakken stuks.
batch-error-empty = Zet minstens één regel op meer dan 0%.
batch-error-not-mixed = Deze batch is nog niet gemengd. Na het mengen vul je de gebakken stuks in.
# $product: Desem wit, $n: 4
batches-unbatched = { $n ->
    [one] 1 stuk { $product } zit nog niet in een batch.
   *[other] { $n } stuks { $product } zitten nog niet in een batch.
}

# Product form: dough per piece, for batches (F14a)
product-dough-weight = Deeg per stuk (gram)
product-dough-weight-hint = Nodig om batches te maken. Leeg of 0 als het product geen deeg heeft.
product-error-dough-weight = Vul het deeggewicht in gram in, van 0 tot 1.000.000.
