-- Every file and folder is a node with a stable id. Its name and path come from its parent
-- chain, so rename, move and trash never rewrite other rows.
CREATE TABLE IF NOT EXISTS nodes (
    id            TEXT PRIMARY KEY,
    parent_id     TEXT REFERENCES nodes(id),
    name          TEXT NOT NULL,
    kind          TEXT NOT NULL CHECK (kind IN ('file', 'folder')),
    owner         TEXT NOT NULL,
    head_revision INTEGER,
    trashed       INTEGER NOT NULL DEFAULT 0,
    starred       INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL,
    modified_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS nodes_parent ON nodes(parent_id);
-- Two live nodes cannot share a name in one folder. Trashed nodes do not count.
CREATE UNIQUE INDEX IF NOT EXISTS nodes_live_name ON nodes(parent_id, name) WHERE trashed = 0;
INSERT OR IGNORE INTO nodes (id, parent_id, name, kind, owner, trashed, starred, created_at, modified_at)
    VALUES ('root', NULL, '', 'folder', 'system', 0, 0, '1970-01-01T00:00:00.000Z', '1970-01-01T00:00:00.000Z');

-- Immutable revisions of a file's content. Rows are only ever inserted.
CREATE TABLE IF NOT EXISTS revisions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    node_id    TEXT NOT NULL REFERENCES nodes(id),
    revision   INTEGER NOT NULL,
    hash       TEXT NOT NULL,
    size       INTEGER NOT NULL,
    mime       TEXT NOT NULL,
    author     TEXT NOT NULL,
    comment    TEXT NOT NULL,
    properties TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    UNIQUE (node_id, revision)
);

-- Role grants on a node. Grants apply to everything below the node. Roles: reader < writer < owner.
CREATE TABLE IF NOT EXISTS permissions (
    node_id   TEXT NOT NULL REFERENCES nodes(id),
    principal TEXT NOT NULL,
    role      TEXT NOT NULL CHECK (role IN ('reader', 'writer', 'owner')),
    PRIMARY KEY (node_id, principal)
);

-- Bearer-capability download links. Persist only the hash of the secret.
-- Deleting a node permanently cascades to its links.
CREATE TABLE IF NOT EXISTS download_links (
    id          TEXT PRIMARY KEY,
    node_id     TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    token_hash  TEXT NOT NULL UNIQUE,
    created_at  TEXT NOT NULL,
    expires_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS download_links_node ON download_links(node_id);

-- Resumable uploads; parts are stored under transfers/<id>.part.
CREATE TABLE IF NOT EXISTS transfers (
    id               TEXT PRIMARY KEY,
    path             TEXT NOT NULL,
    size             INTEGER NOT NULL,
    received         INTEGER NOT NULL DEFAULT 0,
    actor            TEXT NOT NULL,
    sha256           TEXT,
    if_head_revision INTEGER,
    expires_at       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS transfers_expiry ON transfers(expires_at);
