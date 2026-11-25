# Product labels (F18) and freezer labels (F18a) for the Niimbot app
labels-ingredients = Ingrediënten:
# $allergens: melk, noten
labels-may-contain = Kan sporen bevatten van { $allergens }.
labels-net-weight = Netto gewicht:
# $grams: 800
labels-grams = { $grams } g
labels-best-before = Ten minste houdbaar tot:
labels-frozen-on = Ingevroren op:
labels-batch = Partij:
labels-freezer-storage = Bewaren in de vriezer bij −18 °C.
# $name: Brood voor Weldadigheid; $address: Dorpsstraat 1, Frederiksoord
labels-business = { $name }, { $address }
labels-kind-product = Productetiket
labels-kind-freezer = Vriezeretiket
labels-csv-file = Bestand
labels-csv-kind = Soort
labels-csv-product = Product
labels-csv-ingredients = Ingrediënten
labels-csv-may-contain = Kan sporen bevatten van
labels-csv-net-weight = Netto gewicht
labels-csv-best-before = Ten minste houdbaar tot
labels-csv-frozen-on = Ingevroren op
labels-csv-storage = Bewaren
labels-csv-batch = Partij
labels-csv-business = Bedrijf
labels-csv-copies = Aantal

# Back office: the company address for labels
settings-address-title = Adres op etiketten
settings-address = Adres
settings-address-hint = Straat, postcode en plaats op één regel. Zonder adres kun je geen etiketten maken.
# $max: 200
settings-address-too-long = Gebruik hoogstens { $max } tekens.

# Back office: a product's label settings
# $product: Granola
product-labels-title = Etiket van { $product }
product-labels-link = Etiketgegevens
product-labels-intro = Wat er naast de allergenen en ingrediënten op het etiket komt. Een productetiket heeft houdbaarheid en bewaaradvies nodig; een vriezeretiket een vriestermijn.
product-labels-packaged = Verpakt verkocht (krijgt een productetiket)
product-labels-shelf-life = Houdbaarheid (dagen)
product-labels-shelf-life-hint = Ten minste houdbaar tot = bakdag plus dit aantal dagen.
product-labels-storage = Bewaaradvies
product-labels-storage-hint = Bijvoorbeeld: Droog en koel bewaren.
product-labels-freezer-days = Vriestermijn (dagen)
product-labels-freezer-days-hint = Ten minste houdbaar tot op het vriezeretiket = invriesdatum plus dit aantal dagen. Leeg = geen vriezeretiket.
# $max: 3650
product-labels-days-invalid = Vul een heel aantal dagen in, van 1 tot { $max }.
# $max: 200
product-labels-storage-too-long = Gebruik hoogstens { $max } tekens.
product-labels-back = Terug naar het product

# Back office: a batch's label files
batch-labels-link = Etiketten voor de Niimbot
# $code: 261205-2
batch-labels-title = Etiketten van batch { $code }
batch-labels-intro = Download de PNG en print hem met de Niimbot-app, of importeer de CSV in de app. Het aantal staat erbij.
batch-labels-check = Allergenen en ingrediënten van deze batch bekijken
batch-labels-product = Productetiket
batch-labels-freezer = Vriezeretiket
# $n: 8
batch-labels-copies = Aantal: { $n }
# $width: 50; $length: 30
batch-labels-file-whole = PNG, { $width } × { $length } mm
batch-labels-file-front = PNG voorkant, { $width } × { $length } mm
batch-labels-file-back = PNG achterkant, { $width } × { $length } mm
batch-labels-file-csv = CSV voor de import in de Niimbot-app
batch-labels-frozen = Stuks in de vriezer
batch-labels-frozen-hint = Goede stuks uit deze batch die de vriezer in gaan; één vriezeretiket per stuk. 0 haalt ze weg.
batch-labels-error-frozen = Vul een heel aantal in, niet meer dan de goede stuks.
batch-labels-error-fewer-than-frozen = Er liggen meer stuks in de vriezer dan er dan goed zijn. Pas eerst het aantal in de vriezer aan.
batch-labels-gap-address = Er staat nog geen adres bij de instellingen. Er kan geen etiket geprint worden.
batch-labels-gap-packaged = Dit product wordt niet verpakt verkocht. Zet dat aan bij de etiketgegevens van het product.
batch-labels-gap-shelf-life = Vul eerst de houdbaarheid in bij de etiketgegevens van het product.
batch-labels-gap-storage = Vul eerst het bewaaradvies in bij de etiketgegevens van het product.
batch-labels-gap-freezer-days = Vul eerst de vriestermijn in bij de etiketgegevens van het product.
batch-labels-gap-not-baked = Vul eerst de stuks uit de oven in bij de batch.
batch-labels-gap-no-pieces = Deze batch heeft geen goede stuks.
batch-labels-gap-no-frozen = Vul hieronder in hoeveel stuks de vriezer in gaan.
batch-labels-too-long = De tekst past niet op het langste etiket, ook niet verdeeld over voor- en achterkant.
batch-labels-too-wide = Een woord op het etiket is te lang voor de breedte. Maak de productnaam of de etiketnaam van het ingrediënt korter.
batch-labels-unprintable = Er staat een teken op het etiket dat niet geprint kan worden, zoals een emoji. Pas de productnaam, de ingrediënten of het bewaaradvies aan.
