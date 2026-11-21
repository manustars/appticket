-- A proposal's rationale is written in every language of the UI, so each agent reads it in the
-- language they chose: {"en": "...", "it": "..."}.
ALTER TABLE proposals ALTER COLUMN rationale DROP DEFAULT;
ALTER TABLE proposals
    ALTER COLUMN rationale TYPE jsonb
    USING CASE WHEN rationale = '' THEN '{}'::jsonb ELSE jsonb_build_object('en', rationale) END;
ALTER TABLE proposals ALTER COLUMN rationale SET DEFAULT '{}';
ALTER TABLE proposals ADD CONSTRAINT proposals_rationale_is_object
    CHECK (jsonb_typeof(rationale) = 'object');
