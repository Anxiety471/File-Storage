CREATE TABLE IF NOT EXISTS folders (
    path       TEXT PRIMARY KEY,
    parent     TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS folders_parent ON folders(parent);

CREATE TABLE IF NOT EXISTS files (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    path               TEXT NOT NULL UNIQUE,
    parent             TEXT NOT NULL,
    current_generation INTEGER NOT NULL,
    deleted            INTEGER NOT NULL DEFAULT 0,
    created_at         TEXT NOT NULL,
    updated_at         TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS files_parent ON files(parent);

-- Immutable generations: rows are only ever inserted.
CREATE TABLE IF NOT EXISTS versions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    file_id    INTEGER NOT NULL REFERENCES files(id),
    generation INTEGER NOT NULL,
    hash       TEXT NOT NULL,
    size       INTEGER NOT NULL,
    mime       TEXT NOT NULL,
    author     TEXT NOT NULL,
    comment    TEXT NOT NULL,
    metadata   TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    UNIQUE (file_id, generation)
);
