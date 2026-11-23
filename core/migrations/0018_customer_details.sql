-- A customer's details (docs/domain-rules.md, "Customers, assets and services"): beside its name,
-- optionally a legal name, a tax identifier with its country, an address, a phone number, a
-- website and internal notes. A blank detail is no detail: every one is kept trimmed.

ALTER TABLE customers
    ADD COLUMN legal_name text CHECK (legal_name ~ '^\S(.*\S)?$'),
    ADD COLUMN tax_id text CHECK (tax_id ~ '^\S(.*\S)?$'),
    -- The country that issued the tax identifier: an ISO 3166-1 alpha-2 code such as IT.
    ADD COLUMN tax_country text CHECK (tax_country ~ '^[A-Z]{2}$'),
    ADD COLUMN address text CHECK (address ~ '^\S(.*\S)?$'),
    ADD COLUMN phone text CHECK (phone ~ '^\S(.*\S)?$'),
    -- Shown as a link, so only the web's own schemes.
    ADD COLUMN website text CHECK (website ~ '^https?://[^\s/?#]+\S*$'),
    ADD COLUMN notes text NOT NULL DEFAULT '' CHECK (notes = '' OR notes ~ '^\S(.*\S)?$'),
    -- A tax identifier always comes with its country, and a country with an identifier.
    ADD CONSTRAINT customers_tax_id_with_country
        CHECK ((tax_id IS NULL) = (tax_country IS NULL));
