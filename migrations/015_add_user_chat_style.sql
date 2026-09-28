-- How a registered user's name and messages look to everyone in a room: a
-- palette color (NULL for the automatic color clients derive from the name)
-- and one of three treatments. The server validates colors against its palette.
ALTER TABLE users
    ADD COLUMN chat_color VARCHAR(16),
    ADD COLUMN chat_style VARCHAR(16) NOT NULL DEFAULT 'accent',
    ADD CONSTRAINT users_chat_color_format CHECK (chat_color IS NULL OR chat_color ~ '^[a-z]{3,12}$'),
    ADD CONSTRAINT users_chat_style_value CHECK (chat_style IN ('accent', 'text', 'bubble'));
