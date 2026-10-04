-- Independent profile and room header appearance. Chat message styles stay unchanged.
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '10min';

ALTER TABLE users
    ADD COLUMN profile_style JSONB NOT NULL DEFAULT '{"color":null,"style":"accent"}'::jsonb;

ALTER TABLE users
    ADD CONSTRAINT users_profile_style_valid CHECK (
        jsonb_typeof(profile_style) = 'object'
        AND octet_length(profile_style::text) <= 128
        AND profile_style ?& ARRAY['color', 'style']
        AND profile_style - 'color' - 'style' = '{}'::jsonb
        AND jsonb_typeof(profile_style -> 'style') = 'string'
        AND profile_style ->> 'style' IN ('accent', 'text', 'bubble')
        AND (
            profile_style -> 'color' = 'null'::jsonb
            OR profile_style ->> 'color' IN (
                'rose', 'red', 'orange', 'amber', 'lime', 'green', 'emerald', 'teal',
                'cyan', 'sky', 'blue', 'indigo', 'violet', 'purple', 'fuchsia', 'pink'
            )
        )
    ) NOT VALID;

ALTER TABLE rooms
    ADD COLUMN name_style JSONB NOT NULL DEFAULT '{"color":null,"style":"accent"}'::jsonb,
    ADD COLUMN topic_style JSONB NOT NULL DEFAULT '{"color":null,"style":"accent"}'::jsonb;

ALTER TABLE rooms
    ADD CONSTRAINT rooms_name_style_valid CHECK (
        jsonb_typeof(name_style) = 'object'
        AND octet_length(name_style::text) <= 128
        AND name_style ?& ARRAY['color', 'style']
        AND name_style - 'color' - 'style' = '{}'::jsonb
        AND jsonb_typeof(name_style -> 'style') = 'string'
        AND name_style ->> 'style' IN ('accent', 'text', 'bubble')
        AND (
            name_style -> 'color' = 'null'::jsonb
            OR name_style ->> 'color' IN (
                'rose', 'red', 'orange', 'amber', 'lime', 'green', 'emerald', 'teal',
                'cyan', 'sky', 'blue', 'indigo', 'violet', 'purple', 'fuchsia', 'pink'
            )
        )
    ) NOT VALID;

ALTER TABLE rooms
    ADD CONSTRAINT rooms_topic_style_valid CHECK (
        jsonb_typeof(topic_style) = 'object'
        AND octet_length(topic_style::text) <= 128
        AND topic_style ?& ARRAY['color', 'style']
        AND topic_style - 'color' - 'style' = '{}'::jsonb
        AND jsonb_typeof(topic_style -> 'style') = 'string'
        AND topic_style ->> 'style' IN ('accent', 'text', 'bubble')
        AND (
            topic_style -> 'color' = 'null'::jsonb
            OR topic_style ->> 'color' IN (
                'rose', 'red', 'orange', 'amber', 'lime', 'green', 'emerald', 'teal',
                'cyan', 'sky', 'blue', 'indigo', 'violet', 'purple', 'fuchsia', 'pink'
            )
        )
    ) NOT VALID;
