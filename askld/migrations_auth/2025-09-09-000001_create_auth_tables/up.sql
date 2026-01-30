CREATE TABLE IF NOT EXISTS users
(
    id UUID NOT NULL PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS api_keys
(
    id UUID NOT NULL PRIMARY KEY,
    user_id UUID NOT NULL,
    hashed_secret TEXT NOT NULL,
    name TEXT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    last_used_at TIMESTAMPTZ NULL,
    revoked_at TIMESTAMPTZ NULL,
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS api_keys_user_id_idx ON api_keys(user_id);
