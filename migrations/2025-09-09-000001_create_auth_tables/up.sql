CREATE SCHEMA IF NOT EXISTS auth;

CREATE TABLE IF NOT EXISTS auth.users
(
    id UUID NOT NULL PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS auth.api_keys
(
    id UUID NOT NULL PRIMARY KEY,
    user_id UUID NOT NULL,
    hashed_secret TEXT NOT NULL,
    name TEXT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    last_used_at TIMESTAMPTZ NULL,
    revoked_at TIMESTAMPTZ NULL,
    expires_at TIMESTAMPTZ NULL,
    FOREIGN KEY (user_id) REFERENCES auth.users(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS api_keys_user_id_idx ON auth.api_keys(user_id);
CREATE INDEX IF NOT EXISTS api_keys_revoked_at_idx ON auth.api_keys(revoked_at);
