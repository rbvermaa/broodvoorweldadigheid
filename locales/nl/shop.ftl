# Webshop: drop calendar, menu, cart, checkout and order status (F1a, F4, F4b, F4c, F5)
drops-title = Bread Drops
drop-heading = Bread Drop { $date }
drops-intro = Hier zie je wanneer we bakken. Bestel voor de bestel-deadline en haal je brood op tijdens het ophaalmoment.
drops-empty-title = Nog geen Bread Drops gepland
drops-empty-text = Kijk binnenkort nog eens.
drops-ical = Zet de Bread Drops in je agenda
drop-status-open = Open
drop-status-sold-out = Uitverkocht
drop-status-closed = Gesloten
drop-status-cancelled = Gaat niet door
drop-label-pickup = Ophalen
drop-label-order-until = Bestellen tot
drop-label-order-from = Bestellen vanaf
drop-label-deadline = Bestel-deadline
drop-label-charity = Goed doel
# $when: short date and time, such as "do 10 dec 20:00"
drop-order-until = bestellen tot { $when }
drop-order-from = bestellen vanaf { $when }
drop-deadline = bestel-deadline { $when }
drop-for-charity = voor { $charity }
# Pickup moments the list leaves out
drop-more-pickups = { $n ->
    [one] +1 ander moment
   *[other] +{ $n } andere momenten
}
drop-order-button = Bestellen
home-drops-title = Komende Bread Drops
drop-menu = Menu
drop-menu-later = Het menu volgt nog.
drop-not-open = Bestellen kan nu niet voor deze Bread Drop.
drop-back = Alle Bread Drops
cart-product = Product
cart-remove = Weghalen: { $product }
cart-added = In je mandje gedaan.
cart-removed = Uit je mandje gehaald.
cart-replaced = Je mandje had brood van een andere Bread Drop. Dat hebben we eruit gehaald.
cart-error-qty = Kies een aantal van 0 tot en met { $max }.
cart-error-price = Vul een bedrag in, zoals 4,50.
cart-error-price-low = Kies minstens { $min }.
cart-error-price-high = Kies hoogstens { $max }.
cart-error-unavailable = Dit product kun je nu niet bestellen.
cart-line-unavailable = Niet meer beschikbaar
cart-error-closed = Bestellen kan niet meer voor deze Bread Drop.
cart-error-capacity = Er { $n ->
    [one] is er nog maar 1
   *[other] zijn er nog maar { $n }
} over.
product-pwyw-hint = Kies zelf je prijs, tussen { $min } en { $max }. Wij stellen { $suggested } voor.
checkout-title = Afrekenen
checkout-name = Naam
checkout-email = E-mailadres
checkout-email-hint = Hier sturen we je bevestiging naartoe.
checkout-phone = Telefoonnummer
checkout-phone-hint = Niet verplicht. Alleen voor als we je moeten bellen over je bestelling.
checkout-pickup = Wanneer haal je je bestelling op?
checkout-marketing = Mail me als er een nieuwe Bread Drop is
checkout-pay = Betalen
checkout-summary = Je bestelling
checkout-hold = Na het afrekenen houden we je brood 15 minuten voor je vast. Betaal binnen die tijd.
checkout-donation = Waar gaat je donatie naartoe?
checkout-donation-hint = Wil je later kiezen? Dan krijg je een link in je bevestiging. Kies je niet binnen { $months } maanden, dan gaat je donatie naar het goede doel van de eerstvolgende Bread Drop.
checkout-donation-drop = { $charity } (het goede doel van deze Bread Drop)
checkout-donation-save = Bewaar mijn donatie en kies later
checkout-error-name = Vul je naam in.
checkout-error-email = Vul een geldig e-mailadres in.
checkout-error-phone = Dit telefoonnummer klopt niet.
checkout-error-pickup = Kies een ophaalmoment.
checkout-error-slot-full = Dit ophaalmoment is vol. Kies een ander.
checkout-error-deadline = De bestel-deadline is voorbij. Voor deze Bread Drop kun je niet meer bestellen.
checkout-error-replace-deadline = De bestel-deadline is voorbij, dus je bestelling kun je niet meer wijzigen. Je eerdere bestelling kun je nog ongewijzigd betalen tot je reservering afloopt.
checkout-error-closed = Bestellen kan nu niet voor deze Bread Drop.
checkout-error-capacity = Van { $product } { $n ->
    [one] is er nog maar 1
   *[other] zijn er nog maar { $n }
} over. Pas je mandje aan.
checkout-error-unavailable = Iets in je mandje kun je niet meer bestellen. Pas je mandje aan.
checkout-error-free = Je mandje kost € 0. Dat kun je niet online afrekenen. Kies ook iets met een prijs.
checkout-error-payment = Betalen lukt nu even niet. Probeer het over een paar minuten opnieuw.
checkout-error-form = Kijk je gegevens nog even na.
checkout-error-donation = Kies een goed doel uit de lijst.
payment-description = Bestelling { $number } bij { app-name }
refund-description = Terugbetaling bestelling { $number } bij { app-name }
order-title = Je bestelling
order-number = Bestelnummer { $number }
order-total = Totaal
order-status-waiting = We wachten op je betaling. Deze pagina ververst vanzelf.
order-status-paid = We hebben je betaling ontvangen. Je krijgt een bevestiging per mail.
order-status-failed = Je betaling is niet gelukt, dus je bestelling gaat niet door.
order-status-expired = Je bestelling is verlopen, want de betaling was niet op tijd binnen.
order-status-cancelled = Deze bestelling is geannuleerd.
order-status-refund = Je betaling is binnen, maar deze bestelling gaat niet door. We betalen het bedrag terug en sturen je daarover een mail.
order-again = Opnieuw bestellen
