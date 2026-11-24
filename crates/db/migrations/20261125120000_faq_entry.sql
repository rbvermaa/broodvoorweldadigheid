-- Questions and answers for /faq and the customer assistant (F4e, ADR 0018).
-- answer_md is the small Markdown subset of catalog::markdown.
CREATE TABLE faq_entry (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    question text NOT NULL CHECK (btrim(question) <> ''),
    answer_md text NOT NULL CHECK (btrim(answer_md) <> ''),
    -- Checked at the end of each statement, so one UPDATE can swap positions.
    position integer NOT NULL CHECK (position >= 0)
        CONSTRAINT faq_entry_position_key UNIQUE DEFERRABLE INITIALLY IMMEDIATE,
    published boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- Placeholder content until the texts of the current site are copied in (ADR 0018).
INSERT INTO faq_entry (question, answer_md, position, published) VALUES
(
    'Hoe bestel en betaal ik?',
    E'Kies in de shop een bakdag en je brood, en bestel voor de **bestel-deadline** die bij de bakdag staat.\n\nJe betaalt meteen online, met iDEAL, Wero, creditcard of Bancontact. Zodra je betaling binnen is, krijg je een bevestiging per mail. Bij het ophalen hoef je niets meer af te rekenen.',
    0,
    true
),
(
    'Waar haal ik mijn brood op?',
    E'Je haalt je bestelling op bij **De Proef**, Majoor van Swietenlaan 15, 8382 CE Frederiksoord.\n\nHet ophaalmoment kies je bij het bestellen. Het staat ook in je bevestigingsmail.',
    1,
    true
),
(
    'Hoe weet ik welke allergenen erin zitten?',
    E'Bij elk brood in de shop staan de allergenen en waar het sporen van kan bevatten.\n\nTwijfel je? Stuur ons dan voor je bestelt een bericht via het contactformulier.',
    2,
    true
),
(
    'Waar gaat mijn geld naartoe?',
    E'Bij een bakdag voor een goed doel gaat **het volledige bedrag** dat je betaalt naar dat goede doel. De bakkerij betaalt zelf de ingrediënten, de btw en de kosten van de betaling.\n\nBij elke bakdag zie je welk goed doel meedoet. #payitforward',
    3,
    true
);
