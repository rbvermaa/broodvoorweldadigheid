# Back office: instructions of a recipe version, as text or steps (F14, ADR 0049)
method-title = Werkwijze
method-none-hint = Optioneel. Schrijf de werkwijze als gewone tekst, of in stappen met duur en temperatuur.
method-to-text = Als tekst
method-to-steps = In stappen
method-remove = Werkwijze weghalen
method-to-text-text = De stappen gaan weg. Je begint met een lege tekst.
method-to-steps-text = De tekst gaat weg. Je begint met een lege lijst stappen.
method-remove-text = De werkwijze van deze versie gaat weg.
method-switch-confirm = Ja, omzetten
method-remove-confirm = Ja, weghalen
method-text-label = Tekst
method-text-hint = Gewone tekst. Een lege regel begint een nieuwe alinea.
method-steps-empty = Nog geen stappen. Voeg hieronder je eerste stap toe.
# $n: 1
method-step-number = Stap { $n }
method-step-new = Nieuwe stap
method-step-add = Stap toevoegen
method-step-name = Naam
method-step-name-hint = Zoals Autolyse, Bulk, Vormen, Rijzen of Bakken.
method-step-duration = Duur (minuten)
method-step-temperature = Temperatuur (°C)
method-step-notes = Notities
method-step-duration-label = Duur
method-step-temperature-label = Temperatuur
# $name: Bulk
method-step-up = { $name } omhoog
method-step-down = { $name } omlaag
method-step-remove = { $name } verwijderen
method-error-step-name = Geef de stap een naam van hoogstens 80 tekens.
method-error-duration = Vul een duur in van 1 tot 10080 minuten, of laat het leeg.
method-error-temperature = Vul een temperatuur in van -30 tot 300 °C, of laat het leeg.
method-error-notes = De notities zijn te lang: hoogstens 1000 tekens.
method-error-step = Deze stap klopt niet. Kijk de velden na.
method-error-steps = Een versie heeft hoogstens 40 stappen.
method-error-text = De tekst is te lang: hoogstens 10.000 tekens.
method-error-stale = De werkwijze is intussen veranderd. Kijk of je wijziging er nog bij moet.
method-error-gone = Deze stap bestaat niet meer.
method-start = Starttijd
# $date: vrijdag 4 december
method-start-hint = Op { $date }. Alleen om te rekenen, er wordt niets opgeslagen.
method-start-error = Vul een tijd in, zoals 09:00.
method-show = Tijden berekenen
method-end = Klaar
method-untimed = { $n ->
    [one] 1 stap heeft geen duur en telt niet mee.
   *[other] { $n } stappen hebben geen duur en tellen niet mee.
}
method-days-later = { $n ->
    [one] +1 dag
   *[other] +{ $n } dagen
}
# $min: 30
method-duration-minutes = { $min } min
# $hours: 4
method-duration-hours = { $hours } u
method-duration-hours-minutes = { $hours } u { $min } min
# $c: 24
method-temperature = { $c } °C
