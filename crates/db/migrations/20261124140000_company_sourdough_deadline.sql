ALTER TABLE company
    ADD COLUMN sourdough_deadline_days_before smallint NOT NULL DEFAULT 2
        CONSTRAINT company_sourdough_deadline_days_before
        CHECK (sourdough_deadline_days_before BETWEEN 0 AND 14),
    ADD COLUMN sourdough_deadline_time time NOT NULL DEFAULT '20:00';
