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
staff-nav-orders = Bestellingen
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
product-min-price-hint = Alleen nodig bij betaal-wat-je-wilt.
product-min-price = Laagste prijs
product-max-price = Hoogste prijs
product-unit-weight = Gewicht per stuk (gram)
product-sold-online = In de webshop
product-active = Actief
product-allergens = Allergenen
product-allergens-hint = Vink aan wat erin zit. Na elke wijziging moet iemand de allergenen opnieuw goedkeuren.
product-ingredient-text = Ingrediënten
product-ingredient-text-hint = Zoals op het etiket. Zet allergenen in hoofdletters.
product-allergen-status = Allergenen
product-approved = Goedgekeurd
product-not-approved = Niet goedgekeurd. Allergenen en ingrediënten komen pas in de webshop na goedkeuring.
product-approved-by = Goedgekeurd door { $name } op { $date } om { $time }.
product-approve-link = Allergenen bekijken en goedkeuren
product-recheck = Je hebt de allergenen of ingrediënten gewijzigd. Keur ze opnieuw goed voordat ze in de webshop komen.
product-error-name = Geef het product een naam.
product-error-vat = Kies 9% of 21% btw.
product-error-pwyw = De laagste prijs moet boven € 0 liggen, en de prijs tussen de laagste en hoogste prijs.
product-error-price-high = Dat is meer dan { $max }. Bedoel je een bedrag met een komma, zoals 4,50?
product-error-weight = Vul het gewicht in gram in, meer dan 0.
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
slot-date = Datum
slot-add = Ophaalmoment toevoegen
slot-orders = { $n ->
    [one] 1 bestelling
   *[other] { $n } bestellingen
}
slot-move = Ophaalmoment verplaatsen
slot-move-text = { $n ->
    [one] Er is al 1 bestelling voor dit ophaalmoment.
   *[other] Er zijn al { $n } bestellingen voor dit ophaalmoment.
} Laat die klanten weten dat de tijd verandert: ze krijgen een mail met het nieuwe ophaalmoment.
slot-move-confirm = Verplaatsen en klanten mailen
slot-remove = Ophaalmoment verwijderen
# $slot: 11:00–12:00, De Proef
slot-remove-text = Het ophaalmoment { $slot } verdwijnt van deze bakdag.
slot-error-order = Het ophaalmoment moet eindigen na het begin.
# $time: 05:00
slot-error-before-bake = Het ophaalmoment kan niet beginnen voor het bakken begint, om { $time } op de bakdag.
slot-error-has-orders = Er zijn bestellingen voor dit ophaalmoment. Verplaats het ophaalmoment of zet de bestellingen eerst over.
day-schedule = Tijden
day-menu-publish = Menu tonen vanaf
day-menu-publish-hint = Leeg laten als het menu nog niet vaststaat.
day-opens = Bestellen opent
day-opens-hint = Leeg laten om zelf te openen met de knop hierboven.
day-cutoff = Bestel-deadline
day-moment = { $date } om { $time }
day-lead-time-warning-sourdough = Let op: de bestel-deadline ({ $cutoff }) is later dan het zuurdesem toelaat. Zet de deadline uiterlijk op { $latest }.
day-charity-dropped = Het goede doel van de oorspronkelijke bakdag is gearchiveerd en is niet meegekopieerd. Kies hieronder een goed doel, of laat het een gewone verkoop.
day-open = Bestellen openen
day-open-text = Bestellen gaat nu open voor { $date }. Klanten kunnen bestellen tot de bestel-deadline.
day-close = Bestellen sluiten
day-close-text = Bestellen sluit nu voor { $date }. Klanten kunnen daarna niet meer bestellen, ook niet voor de bestel-deadline.
day-close-production = Bakdag afsluiten
day-close-production-text = { $date } wordt afgesloten. Je kunt daarna geen tijden of aanbod meer instellen voor bestellingen; extra productie blijft mogelijk.
day-copy-title = Kopiëren
day-copy-date = Kopiëren naar datum
day-copy-hint = Tijden, ophaalmomenten en aanbod gaan mee. Extra productie niet.
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
day-error-deadline-after-bake = De bestel-deadline moet uiterlijk om { $time } op de bakdag liggen. Dan begint het bakken.
day-error-production-only-open = Sluit eerst het bestellen voordat je er een bakdag voor alleen productie van maakt.
day-error-weeks = Vul een aantal weken in van 1 tot en met 52.
day-error-no-pickup-slot = Deze bakdag heeft geen ophaalmoment. Zonder ophaalmoment kan hij niet in de webshop.
range-title = Aanbod
range-empty = Nog geen producten op deze bakdag.
range-product = Product
range-product-hint = Hier staan alleen producten die nog niet in het aanbod staan. De grenzen van de andere wijzig je hierboven.
range-max-qty = Maximaal aantal van dit brood
range-max-qty-hint = Leeg laten voor geen maximum.
range-add = In het aanbod zetten
range-no-max = geen maximum
range-max = maximaal { $n } van dit brood
range-remaining = { $n ->
    [one] nog 1 over
   *[other] nog { $n } over
}
range-unlimited = onbeperkt
range-remove = Uit het aanbod halen
range-remove-confirm = Ja, uit het aanbod
range-remove-text = { $product } verdwijnt uit het aanbod van { $date }. Klanten kunnen het dan niet meer bestellen voor deze bakdag.
range-error-product = Kies een product.

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

# Back office: orders (F6)
staff-orders-days-caption = Bakdagen
staff-orders-days-empty = Er zijn geen bakdagen in deze periode.
staff-orders-day-heading = Bestellingen voor { $date }
staff-orders-back = Andere bakdag kiezen
staff-orders-caption = Bestellingen
staff-orders-filter = Filteren
staff-orders-filter-any = Alle
staff-orders-filter-location = Ophaallocatie
staff-orders-empty-title = Geen bestellingen
staff-orders-empty-text = Er zijn geen bestellingen die bij je filter passen.
staff-order-heading = Bestelling { $number }
staff-order-back = Terug naar de bestellingen
staff-order-number = Bestelnummer
staff-order-customer = Klant
staff-order-email = E-mail
staff-order-phone = Telefoon
staff-order-pickup = Ophaalmoment
staff-order-pieces = Stuks
staff-order-total = Totaal
staff-order-status = Status
staff-order-status-pending_payment = Wacht op betaling
staff-order-status-paid = Betaald
staff-order-status-confirmed = Bevestigd
staff-order-status-picked_up = Opgehaald
staff-order-status-cancelled = Geannuleerd
staff-order-status-refunded = Terugbetaald
staff-order-lines-caption = Producten
staff-order-product = Product
staff-order-qty = Aantal
staff-order-price = Prijs per stuk
staff-order-payments-caption = Betalingen
staff-order-payment-reference = Kenmerk
staff-order-payment-amount = Bedrag
staff-order-payment-ok = Ontvangen
staff-order-payment-refund-due = Nog terug te betalen
staff-order-payment-refunded = { $amount } terugbetaald
staff-order-payment-refund-requested = Wordt terugbetaald
staff-order-move = Ander ophaalmoment
staff-order-move-save = Verplaatsen
staff-order-picked-up = Opgehaald
staff-order-picked-up-text = Bestelling { $number } van { $name } wordt als opgehaald gemarkeerd.
staff-order-cancel = Bestelling annuleren
staff-order-cancel-text = Bestelling { $number } van { $name } wordt geannuleerd. De klant krijgt een mail.
staff-order-cancel-paid-text = Bestelling { $number } van { $name } wordt geannuleerd. We betalen { $amount } terug via Mollie en de klant krijgt een mail.
staff-order-refunds-caption = Terugbetalingen
staff-order-refund-requested = Nog niet verstuurd
staff-order-refund-done = Verstuurd naar Mollie
staff-order-refund-failed = Mislukt
staff-order-refund-retry = Terugbetaling opnieuw versturen
staff-order-refund-retry-text = We vragen Mollie opnieuw om { $amount } terug te betalen voor bestelling { $number }.
staff-order-error-status = Dat kan niet meer: de status van deze bestelling is intussen veranderd.
staff-order-error-day = Deze bakdag is klaar of geannuleerd, dus deze bestelling kan niet meer veranderen.
staff-order-error-slot = Kies een ophaalmoment van deze bakdag.
staff-order-error-slot-full = Dit ophaalmoment is vol. Kies een ander.
staff-order-error-invoiced = Deze bestelling is al gefactureerd, dus het ophaalmoment kan niet meer veranderen.
staff-order-history-caption = Geschiedenis
staff-order-history-when = Wanneer
staff-order-history-who = Wie
staff-order-history-what = Wat
staff-order-actor-customer = Klant
staff-order-actor-system = Systeem
staff-order-action-created = Besteld
staff-order-action-paid = Betaald
staff-order-action-payment_refused = Betaling niet aangenomen
staff-order-action-payment_status = Betaalstatus bijgewerkt
staff-order-action-hold_expired = Verlopen zonder betaling
staff-order-action-cancelled = Geannuleerd
staff-order-action-picked_up = Opgehaald
staff-order-action-pickup_moved = Ophaalmoment veranderd
staff-order-action-invoiced = Gefactureerd
staff-order-action-refund_done = Terugbetaling verstuurd
staff-order-action-refund_failed = Terugbetaling mislukt
staff-orders-cancel-day = Bakdag afzeggen
staff-day-cancel-sick = Bakdag annuleren (ziek)
staff-sick-day-heading = Bakdag van { $date } afzeggen
staff-sick-day-orders = Bestellingen die we annuleren
staff-sick-day-refunds = Bestellingen die geld terugkrijgen
staff-sick-day-amount = Totaal terug te betalen
staff-sick-day-warning = De bakdag gaat dicht en elke open bestelling wordt geannuleerd. Betaalde bestellingen krijgen hun geld terug via Mollie, en elke klant krijgt een mail met je bericht. Dit kun je niet terugdraaien.
staff-sick-day-message = Je bericht aan de klanten
staff-sick-day-message-hint = Dit staat in de mail aan elke klant.
staff-sick-day-confirm = Bakdag afzeggen
staff-sick-day-error-message = Schrijf een bericht voor je klanten.
staff-sick-day-error-done = Deze bakdag is al klaar, dus je kunt hem niet meer afzeggen.
staff-sick-day-cancelled = Deze bakdag is afgezegd. Alle bestellingen zijn geannuleerd en de klanten krijgen een mail.
staff-sick-day-open-refunds = Terugbetalingen die nog niet gelukt zijn
staff-sick-day-retry = Terugbetalingen opnieuw versturen
staff-sick-day-retry-text = We vragen Mollie opnieuw om de terugbetalingen hierboven te versturen. Wat al verstuurd is, gaat niet nog een keer.
staff-sick-day-all-done = Alle terugbetalingen zijn verstuurd naar Mollie.
staff-sick-day-invoiced = Deze bestellingen zijn al gefactureerd en blijven staan: { $numbers }. De klant krijgt wel de mail. Pas de factuur zelf aan.
