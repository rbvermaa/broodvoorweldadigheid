staff-title = Medewerkers
staff-signed-in-as = Je bent ingelogd als
staff-role-owner = eigenaar
staff-role-baker = bakker
staff-role-counter = toonbank
staff-intro = Kies hierboven wat je wilt beheren.
staff-logout = Uitloggen
staff-not-allowed-title = Geen toegang
staff-not-allowed-text = Dit account hoort niet bij een medewerker. Vraag de eigenaar om je toe te voegen.
staff-forbidden-title = Geen toegang
staff-forbidden-text = Je rol geeft geen toegang tot deze pagina. Vraag de eigenaar als je dit nodig hebt.
staff-unavailable-title = Inloggen kan nog niet
staff-unavailable-text = Inloggen voor medewerkers is nog niet beschikbaar.
staff-dev-login-title = Inloggen voor ontwikkelaars
staff-dev-login-text = Alleen voor lokaal testen. Je logt in als de medewerker met dit nummer, zonder wachtwoord.
staff-dev-login-id = Nummer van de medewerker
staff-dev-login-submit = Inloggen

# Back office: shared
staff-nav-label = Beheer
staff-nav-bake-days = Bakdagen
staff-nav-products = Producten
staff-nav-locations = Ophaallocaties
staff-nav-charities = Goede doelen
form-save = Opslaan
form-yes = Ja
form-no = Nee
form-has-errors = Niet alles is goed ingevuld. Kijk bij de velden hieronder.
form-error-money = Vul een bedrag in, bijvoorbeeld 6,50.
form-error-count = Vul een heel getal in van 0 of meer.
form-error-time = Vul een tijd in, bijvoorbeeld 11:00.
form-error-date = Vul een datum in.
form-error-datetime = Vul een datum en tijd in.

# Back office: products (F10)
products-caption = Alle producten
products-empty-title = Nog geen producten
products-empty-text = Voeg je eerste brood toe.
product-new = Nieuw product
product-name = Naam
product-description = Omschrijving
product-price = Prijs incl. btw
product-price-hint = Bij betaal-wat-je-wilt is dit het voorgestelde bedrag.
product-vat = Btw
product-pwyw-legend = Betaal wat je wilt
product-pwyw = Klant kiest zelf de prijs
product-pwyw-hint = Alleen nodig bij betaal-wat-je-wilt.
product-min-price = Laagste prijs
product-max-price = Hoogste prijs
product-unit-weight = Gewicht per stuk (gram)
product-lead-time = Rijstijd zuurdesem (uren)
product-lead-time-hint = Leeg laten voor de standaard van de bakkerij.
product-sold-online = In de webshop
product-active = Actief
product-allergens = Allergenen
product-allergens-hint = Vink aan wat erin zit. Na elke wijziging moet iemand de allergenen opnieuw goedkeuren.
product-ingredients = Ingrediënten
product-ingredients-hint = Zoals op het etiket. Zet allergenen in hoofdletters.
product-allergen-status = Allergenen
product-approved = Goedgekeurd
product-not-approved = Niet goedgekeurd. Allergenen en ingrediënten komen pas in de webshop na goedkeuring.
product-approved-by = Goedgekeurd door { $name } op { $date } om { $time }.
product-approve-link = Allergenen bekijken en goedkeuren
product-recheck = Je hebt de allergenen of ingrediënten gewijzigd. Keur ze opnieuw goed voordat ze in de webshop komen.
product-error-name = Geef het product een naam.
product-error-vat = Kies 9% of 21% btw.
product-error-pwyw = De laagste prijs moet boven € 0 liggen, en de prijs tussen de laagste en hoogste prijs.
product-error-weight = Vul het gewicht in gram in, meer dan 0.
product-error-lead-time = Vul een aantal uren in tussen 0 en 168, of laat het leeg.
approve-title = Allergenen goedkeuren
approve-intro = Je keurt precies dit goed. Daarna ziet de klant het in de webshop.
approve-no-allergens = Bevat geen van de 14 allergenen.
approve-no-may-contain = Geen sporen van allergenen.
approve-no-ingredients = Nog geen ingrediënten ingevuld.
approve-submit = Ja, dit klopt
approve-changed = De allergenen zijn net gewijzigd. Bekijk ze hieronder opnieuw voordat je goedkeurt.
approve-back = Terug naar het product

# Back office: charities (F4g)
charities-caption = Alle goede doelen
charities-empty-title = Nog geen goede doelen
charities-empty-text = Voeg het eerste goede doel toe.
charity-new = Nieuw goed doel
charity-name = Naam
charity-description = Korte omschrijving
charity-url = Website
charity-url-hint = Het volledige adres, met https://
charity-iban = IBAN voor overmakingen
charity-anbi = ANBI-nummer (RSIN)
charity-status = Status
charity-active = Actief
charity-archived = Gearchiveerd
charity-is-archived = Dit goede doel is gearchiveerd. Klanten kunnen het niet meer kiezen. Eerdere drops en totalen blijven bewaard.
charity-archive = Archiveren
charity-archive-confirm = Ja, archiveren
charity-archive-text = { $name } wordt gearchiveerd. Klanten en nieuwe drops kunnen het dan niet meer kiezen. Drops die het al hebben, en alle totalen, blijven bewaard.
charity-error-name = Geef het goede doel een naam.
charity-error-url = Vul het webadres in, beginnend met https://

# Back office: pickup locations (F4a)
locations-caption = Alle ophaallocaties
locations-empty-title = Nog geen ophaallocaties
locations-empty-text = Voeg de plek toe waar klanten hun brood ophalen.
location-new = Nieuwe ophaallocatie
location-name = Naam
location-address = Adres
location-notes = Opmerkingen
location-error-name = Geef de locatie een naam.
location-error-address = Vul het adres in.
windows-caption = Ophaalvensters
windows-intro = Een nieuwe bakdag krijgt deze ophaalmomenten en bestel-deadline. Per bakdag kun je ze nog aanpassen.
windows-empty-title = Nog geen ophaalvensters
windows-empty-text = Zonder ophaalvenster kun je hier geen bakdag met webshop plannen.
window-new = Nieuw ophaalvenster
window-heading = Ophaalvenster bij { $location }
window-weekday = Dag
window-time = Ophaalmoment
window-starts = Van
window-ends = Tot
window-deadline = Bestel-deadline
window-deadline-rule = { $days } dagen ervoor om { $time }
window-days-before = Bestel-deadline: dagen voor het ophalen
window-days-before-hint = Bijvoorbeeld 2: voor zaterdag sluit bestellen op donderdag.
window-deadline-time = Bestel-deadline: tijd
window-back = Terug naar de locatie
window-error-weekday = Kies een dag.
window-error-order = Het ophaalmoment moet eindigen na het begin.
window-error-days = Vul een aantal dagen in van 0 tot en met 28.

# Back office: bake days (F1, F1a, F3, F4c)
days-caption = Komende bakdagen
days-empty-title = Nog geen bakdagen gepland
days-empty-text = Plan je eerste bakdag.
day-new = Nieuwe bakdag
day-new-intro = De ophaalmomenten en de bestel-deadline komen uit de ophaalvensters van de locatie. Daarna kun je ze per bakdag aanpassen.
day-create = Bakdag maken
day-date = Datum
day-location = Ophaallocatie
day-status = Status
day-kind = Soort
day-charity = Goed doel
day-charity-hint = Kies niets voor een gewone verkoop zonder goed doel. Na de eerste bestelling kan dit niet meer veranderen.
day-no-charity = Gewone verkoop
day-online = Webshop
day-production-only = Alleen productie
day-production-only-label = Alleen productie, niet in de webshop
day-bake-starts = Begin van het bakken
day-bake-starts-hint = Datum en tijd waarop het deeg de oven in gaat.
day-status-draft = Concept
day-status-open = Open voor bestellingen
day-status-closed = Gesloten
day-status-cancelled = Geannuleerd
day-status-done = Klaar
day-repeated = { $n ->
    [one] 1 bakdag gemaakt.
   *[other] { $n } bakdagen gemaakt.
}
day-final = Deze bakdag is klaar of geannuleerd. Je kunt hem niet meer wijzigen.
day-slots = Ophaalmomenten
day-no-slots = Geen ophaalmomenten: deze bakdag is alleen voor productie.
day-schedule = Tijden
day-menu-publish = Menu tonen vanaf
day-menu-publish-hint = Leeg laten als het menu nog niet vaststaat.
day-opens = Bestellen opent
day-opens-hint = Leeg laten om zelf te openen met de knop hierboven.
day-cutoff = Bestel-deadline
day-moment = { $date } om { $time }
day-lead-time-warning = Let op: de bestel-deadline ({ $cutoff }) laat te weinig tijd voor het zuurdesem van { $product } ({ $hours } uur). Zet de deadline uiterlijk op { $latest }.
day-charity-dropped = Het goede doel van de oorspronkelijke bakdag is gearchiveerd en is niet meegekopieerd. Kies hieronder een goed doel, of laat het een gewone verkoop.
day-open = Bestellen openen
day-open-text = Bestellen gaat nu open voor { $date }. Klanten kunnen bestellen tot de bestel-deadline.
day-close = Bestellen sluiten
day-close-text = Bestellen sluit nu voor { $date }. Klanten kunnen daarna niet meer bestellen, ook niet voor de bestel-deadline.
day-close-production = Bakdag afsluiten
day-close-production-text = { $date } wordt afgesloten. Je kunt daarna geen tijden of aanbod meer instellen voor bestellingen; extra productie blijft mogelijk.
day-copy-title = Kopiëren
day-copy-date = Kopiëren naar datum
day-copy-hint = Tijden, ophaalmomenten, aanbod en pools gaan mee. Extra productie niet.
day-copy = Kopiëren
day-repeat-weeks = Elke week herhalen, aantal weken
day-repeat-hint = Maakt een kopie op dezelfde dag in elk van de komende weken (1 tot 52).
day-repeat = Herhalen
day-error-location = Kies een ophaallocatie.
day-error-no-window = Deze locatie heeft geen actief ophaalvenster op { $weekday }. Voeg eerst een ophaalvenster toe, of kies alleen productie.
day-error-date-taken = Er is al een bakdag op { $date } bij deze ophaallocatie.
day-error-charity-unknown = Kies een goed doel uit de lijst.
day-error-charity-archived = Dit goede doel is gearchiveerd. Kies een ander goed doel.
day-error-orders-exist = Er zijn al bestellingen voor deze bakdag. Het goede doel kan niet meer veranderen.
day-error-final = Deze bakdag is klaar of geannuleerd. Je kunt hem niet meer wijzigen.
day-error-transition = Dat kan niet in de huidige status van deze bakdag.
day-error-production-only = Een bakdag voor alleen productie gaat nooit open in de webshop.
day-error-deadline-passed = De bestel-deadline is al voorbij. Zet eerst een latere bestel-deadline.
day-error-not-baked = Het bakken is nog niet begonnen.
day-error-deadline-missing = Een bakdag in de webshop heeft een bestel-deadline nodig.
day-error-opens-production-only = Een bakdag voor alleen productie gaat niet open. Laat dit leeg.
day-error-opens-after-deadline = Bestellen moet openen voor de bestel-deadline.
day-error-menu-after-deadline = Het menu moet zichtbaar zijn voor de bestel-deadline.
day-error-deadline-after-bake = De bestel-deadline moet voor het begin van het bakken liggen.
day-error-production-only-open = Sluit eerst het bestellen voordat je er een bakdag voor alleen productie van maakt.
day-error-weeks = Vul een aantal weken in van 1 tot en met 52.
range-title = Aanbod
range-empty = Nog geen producten op deze bakdag.
range-product = Product
range-product-hint = Kies een product dat al in het aanbod staat om de grenzen te wijzigen.
range-max-qty = Maximaal aantal van dit brood
range-max-qty-hint = Leeg laten voor geen maximum.
range-pool = Pool
range-pool-hint = Optioneel: het brood telt dan mee voor de pool, bijvoorbeeld een ovenlading.
range-units = Eenheden per stuk in de pool
range-units-hint = Meestal 1. Een groot brood kan 2 plekken in de oven innemen.
range-add = In het aanbod zetten
range-no-max = geen maximum
range-max = maximaal { $n } van dit brood
range-in-pool = pool { $pool } ({ $units } per stuk)
range-remaining = { $n ->
    [one] nog 1 over
   *[other] nog { $n } over
}
range-unlimited = onbeperkt
range-remove = Uit het aanbod halen
range-remove-confirm = Ja, uit het aanbod
range-remove-text = { $product } verdwijnt uit het aanbod van { $date }. Klanten kunnen het dan niet meer bestellen voor deze bakdag.
range-error-product = Kies een product.
range-error-pool = Kies een pool van deze bakdag.
range-error-units = Vul een heel getal in van 1 of meer.
pools-title = Pools
pools-intro = Een pool is gedeelde ruimte, zoals een deeg of een ovenlading. Producten in een pool delen het maximum.
pool-name = Naam van de pool
pool-name-hint = Bijvoorbeeld Oven 1.
pool-max = Maximaal aantal eenheden
pool-size-hint = Maximaal aantal eenheden.
pool-add = Pool toevoegen
pool-error-name = Geef de pool een naam.
pool-error-duplicate = Er is al een pool met deze naam op deze bakdag.
pool-error-size = Vul voor de pool een heel getal in van 0 of meer.

# Back office: extra production (F7a)
extras-title = Extra productie
extras-heading = Extra productie voor { $date }
extras-back = Terug naar de bakdag
extras-capacity = Wat er nog past
extras-extra = Extra
extras-left = Nog over
extras-add-title = Extra stuks toevoegen
extras-add = Toevoegen
extra-row-heading = { $product }: { $purpose }
extra-qty = Aantal
extra-purpose = Waarvoor
extra-note = Opmerking
extra-purpose-freezer = Vriezer
extra-purpose-sumup = Verkoop via SumUp
extra-purpose-samples = Proeverij
extra-purpose-other = Anders
extra-remove = Verwijderen
extra-remove-confirm = Ja, verwijderen
extra-remove-text = { $qty } × { $product } ({ $purpose }) gaat van de productie van { $date } af. Die ruimte komt weer vrij in de webshop.
extra-error-qty = Vul een aantal in van 1 of meer.
extra-error-purpose = Kies waarvoor de extra stuks zijn.
extra-error-capacity = { $n ->
    [0] Dat past niet meer: er kan niets meer bij van dit product op deze bakdag.
    [one] Dat past niet: er kan nog maar 1 stuk bij van dit product op deze bakdag.
   *[other] Dat past niet: er kunnen nog maar { $n } stuks bij van dit product op deze bakdag.
}
extra-error-capacity-line = { $n ->
    [0] Dat past niet: er kan niets meer bij van dit product op deze bakdag.
    [one] Dat past niet: deze regel kan hoogstens 1 stuk zijn.
   *[other] Dat past niet: deze regel kan hoogstens { $n } stuks zijn.
}
