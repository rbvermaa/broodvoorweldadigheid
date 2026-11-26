# Back office: batches per bake day (F14a)
batches-title = Batches
# $date: zaterdag 5 december 2026
batches-heading = Batches voor { $date }
batches-back = Terug naar de Bread Drop
batches-caption = Batches van deze Bread Drop
batches-empty-title = Nog geen batches
batches-empty-text = Start hieronder een batch met de hand.
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
# $code: 261205-2
batch-heading = Batch { $code }
batch-back = Alle batches van deze Bread Drop
batch-facts = Over deze batch
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
product-dough-weight-hint = Nodig voor batches. Leeg of 0 als het product geen deeg heeft.
product-error-dough-weight = Vul het deeggewicht in gram in, van 0 tot 1.000.000.

# Batch page: save the batch's formula as the next recipe version (F14b)
batch-promote-title = Nieuwe receptversie
batch-promote-hint = Bevalt deze aanpassing? Sla de regels van deze batch op als de volgende versie van het recept. Die wordt een concept; je neemt hem zelf in gebruik.
batch-promote = Opslaan als nieuwe receptversie
# $n: 4
batch-promoted = Opgeslagen als versie { $n } van het recept.
batch-promoted-link = Naar de receptversie
batch-promote-unchanged = Deze batch is gelijk aan het recept. Er is niets om op te slaan.
# $n: 4
batch-promote-draft-exists = Er is al een concept: versie { $n }. Zet dat eerst vast of pas het aan.
# Version notes of a version saved from a batch. $code: 261205-2, $n: 3
batch-promote-note = Opgeslagen uit batch { $code } (gemaakt met versie { $n }).
batch-promote-reasons = Redenen voor de aanpassing:
