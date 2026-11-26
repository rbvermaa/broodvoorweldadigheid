# Emails (F2, F9, F9a). Values such as amounts, dates and links arrive preformatted.

email-greeting = Hoi { $name },
email-greeting-anonymous = Hoi,
email-signoff = Groetjes van de bakker
email-footer-kvk = KvK { $kvk }

email-label-order = Bestelling
email-label-drop = Bread Drop
email-label-pickup = Ophaalmoment
email-label-location = Locatie
email-label-deadline = Bestel-deadline
email-label-charity = Goed doel
email-normal-sale = Normale verkoop, zonder goed doel
email-moment = { $day }, { $range }
email-deadline = { $day }, { $time }
email-place = { $name }, { $address }

email-lines-product = Product
email-lines-qty = Aantal
email-lines-price = Prijs
email-lines-amount = Bedrag
email-line-your-price = Jouw prijs
email-line = { $qty } × { $product }: { $amount } ({ $price } per stuk)
email-line-pay-what-you-want = { $qty } × { $product }: { $amount } (jouw prijs: { $price } per stuk)
email-item = { $qty } × { $product }
email-total = Totaal
email-vat = BTW { $rate }% over { $net }
email-refund = We storten { $amount } terug. Het kan een paar werkdagen duren voor het op je rekening staat.
email-questions = Vragen? Beantwoord deze mail, dan helpen we je verder.

email-order-confirmation-subject = Je bestelling { $number } is bevestigd
email-order-confirmation-heading = Bedankt voor je bestelling!
email-order-confirmation-intro = We gaan voor je bakken. Hieronder zie je wat je hebt besteld en wanneer je het kunt ophalen.
email-order-confirmation-charity = Alles wat je voor deze Bread Drop betaalt, gaat naar { $charity }.
email-order-confirmation-saved = Je bewaart je donatie voor later. Kies vóór { $date } een goed doel. Kies je niets, dan gaat je donatie naar het goede doel van de eerstvolgende Bread Drop.
email-order-confirmation-choose = Kies een goed doel
email-order-confirmation-status = Bekijk je bestelling

email-on-account-subject = Je bestelling { $number } op rekening is bevestigd
email-on-account-heading = Bedankt voor je bestelling!
email-on-account-intro = We gaan voor je bakken. Hieronder zie je wat je op rekening hebt besteld en wanneer je het kunt ophalen.
email-on-account-invoice = Je hoeft nu niets te betalen. Deze bestelling komt op je volgende factuur.

email-donation-moved-subject = Je bewaarde donatie gaat naar { $charity }
email-donation-moved-heading = Je donatie heeft een bestemming
email-donation-moved-intro = Je bewaarde je donatie van { $amount } bij bestelling { $number } voor later. Je hebt geen goed doel gekozen, dus gaat je donatie nu naar { $charity }, het goede doel van de Bread Drop van { $date }.

email-account-deleted-subject = Je gegevens zijn verwijderd
email-account-deleted-heading = Je account is verwijderd
email-account-deleted-intro = Je vroeg ons om je gegevens te verwijderen. Dat is gedaan: je naam, e-mailadres en telefoonnummer zijn gewist, en je krijgt geen mails meer over Bread Drops.
email-account-deleted-orders = Je bestellingen moeten we 7 jaar bewaren voor de belastingdienst. Er staat niets meer bij wat naar jou wijst. Je donaties blijven bij het goede doel.

email-pickup-reminder-subject = Ophalen: je bestelling { $number }
email-pickup-reminder-heading = Je brood ligt bijna klaar
email-pickup-reminder-intro = Je kunt je bestelling ophalen op { $day } tussen { $start } en { $end }.
email-pickup-reminder-items = Dit haal je op:

email-pickup-moved-subject = Nieuw ophaalmoment voor je bestelling { $number }
email-pickup-moved-heading = Je ophaalmoment is veranderd
email-pickup-moved-intro = We hebben het ophaalmoment van je bestelling { $number } veranderd. Dit is het nieuwe moment:
email-pickup-moved-sorry = Past dit niet? Beantwoord deze mail, dan zoeken we samen een oplossing.

email-order-cancelled-subject = Je bestelling { $number } is geannuleerd
email-order-cancelled-heading = Je bestelling is geannuleerd
email-order-cancelled-intro = Je bestelling { $number } voor de Bread Drop van { $date } is geannuleerd.

email-sick-day-subject = De Bread Drop van { $date } gaat niet door
email-sick-day-heading = Helaas, deze Bread Drop gaat niet door
email-sick-day-intro = De Bread Drop van { $date } gaat niet door, dus we hebben je bestelling { $number } geannuleerd. Dit is het bericht van de bakker:
email-sick-day-sorry = Sorry voor het ongemak. Hopelijk tot de volgende Bread Drop!

email-drop-menu-subject = Het menu voor de Bread Drop van { $date } staat online
email-drop-menu-heading = Het menu staat online
email-drop-menu-intro = Benieuwd wat er op { $date } uit de oven komt? Het menu staat online.
email-drop-open-subject = Bestellen kan: Bread Drop van { $date }
email-drop-open-heading = Je kunt nu bestellen
email-drop-open-intro = Je kunt nu bestellen voor de Bread Drop van { $date }. Op is op!
email-drop-menu = Op het menu:
email-drop-shop = Naar de webshop
email-drop-reason = Je krijgt deze mail omdat je je hebt aangemeld voor Bread Drop-alerts.
email-unsubscribe = Afmelden

email-subscription-subject = Bevestig je aanmelding voor Bread Drop-alerts
email-subscription-heading = Bevestig je aanmelding
email-subscription-intro = Fijn dat je wilt horen wanneer er een nieuwe Bread Drop is! Bevestig je aanmelding via de link hieronder.
email-subscription-confirm = Aanmelding bevestigen
email-subscription-ignore = Heb je je niet aangemeld? Dan kun je deze mail negeren. Je krijgt dan geen mails van ons.

email-login-subject = Je inloglink
email-login-heading = Inloggen
email-login-intro = Je vroeg om een link om in te loggen. De link werkt { $minutes } minuten en maar één keer.
email-login-button = Inloggen
email-login-ignore = Heb je niet om deze link gevraagd? Dan kun je deze mail negeren. Er gebeurt dan niets.

email-vote-subject = Bevestig je stem: { $poll }
email-vote-heading = Bevestig je stem
email-vote-intro = Bedankt voor je stem op { $choice }! Bevestig hem via de knop hieronder, dan telt hij mee. De link werkt 7 dagen, en niet meer als de stemming gesloten is.
email-vote-confirm = Stem bevestigen
email-vote-ignore = Heb je niet gestemd? Dan kun je deze mail negeren. Er telt dan geen stem en je krijgt geen mails van ons.

email-contact-subject = Contactformulier: bericht van { $name }
email-contact-heading = Nieuw bericht via de site
email-contact-greeting = Hoi bakker,
email-contact-intro = Er is een bericht binnengekomen via het contactformulier.
email-label-name = Naam
email-label-email = E-mail
email-contact-reply = Beantwoord deze mail om { $name } antwoord te geven.
email-contact-signoff = Verstuurd via het contactformulier op de site
