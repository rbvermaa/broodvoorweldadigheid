# Production list (F7) and pickup list (F8), on paper
production-title = Productielijst
production-product = Product
production-ordered = Besteld
production-extra-freezer = Vriezer
production-extra-sumup = SumUp
production-extra-samples = Proeverij
production-extra-other = Overig
production-to-bake = Bakken
production-sum = Totaal
production-empty = Nog niets te bakken.

pickup-list-title = Ophaallijst
# $day: za 31 okt, $range: 11:00–12:00, $location: De Proef
pickup-list-slot = { $day }, { $range } · { $location }
pickup-list-number = Bestelling
pickup-list-customer = Naam
pickup-list-items = Producten
pickup-list-status = Status
pickup-list-done = Opgehaald
pickup-list-empty = Geen bestellingen.
# $qty is preformatted: 2
pickup-list-line = { $qty }× { $product }
pickup-list-status-paid = Betaald
pickup-list-status-confirmed = Bevestigd
pickup-list-status-on-account = Op rekening
pickup-list-status-picked-up = Opgehaald

# Ingredient needs (F15), on screen, on paper and as CSV
needs-title = Boodschappenlijst
needs-basis-recipes = Berekend uit de receptversies die in gebruik zijn, voor bestellingen en extra productie.
needs-basis-batches = Na de bestel-deadline: berekend uit de batches. Stuks die nog niet in een batch zitten, tellen mee met de receptversie die in gebruik is.
needs-ingredient = Ingrediënt
needs-amount = Hoeveelheid
needs-sum = Totaal
needs-empty = Nog niets nodig.
needs-doughs = Degen
needs-recipe = Recept
needs-products = Producten
# $recipe: Desem, $version: 3
needs-dough-name = { $recipe } (versie { $version })
# $code: 261031-2, $recipe: Desem, $version: 3
needs-dough-batch = Batch { $code }: { $recipe } (versie { $version })
# $pieces is preformatted: 12
needs-dough-product = { $pieces }× { $product }
needs-missing = Geen recept
needs-missing-intro = Deze producten tellen niet mee in de boodschappenlijst.
needs-product = Product
needs-pieces = Stuks
needs-reason = Waarom
needs-reason-no-recipe = geen recept
needs-reason-no-version = geen versie in gebruik
needs-reason-empty = recept zonder regels of percentages
needs-reason-no-dough-weight = geen deeggewicht per stuk
# $amount is preformatted: 850 or 12,345
needs-grams = { $amount } g
needs-kilograms = { $amount } kg
needs-csv-grams = Gram
# $product: Granola, $pieces: 12, $reason: geen recept
needs-csv-missing = Niet meegeteld ({ $reason }): { $product }, { $pieces } stuks
