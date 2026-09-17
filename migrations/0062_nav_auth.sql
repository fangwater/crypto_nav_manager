CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE TABLE nav_users (
    user_id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    username TEXT NOT NULL UNIQUE CHECK (username ~ '^[a-z0-9_.-]{1,64}$'),
    password_hash TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('admin', 'user')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Bootstrap the first administrator directly in PostgreSQL:
--   INSERT INTO nav_users (username, password_hash, role)
--   VALUES ('admin', crypt('<password>', gen_salt('bf', 10)), 'admin');

CREATE TABLE nav_sessions (
    token_hash TEXT PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES nav_users(user_id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    expires_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX idx_nav_sessions_user_id ON nav_sessions(user_id);
CREATE INDEX idx_nav_sessions_expires_at ON nav_sessions(expires_at);

CREATE TABLE nav_user_strategy_grants (
    user_id BIGINT NOT NULL REFERENCES nav_users(user_id) ON DELETE CASCADE,
    strategy_slug TEXT NOT NULL REFERENCES strategy_envs(slug) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (user_id, strategy_slug)
);
