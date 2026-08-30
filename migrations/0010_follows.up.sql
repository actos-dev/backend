-- Actor'lar arası takip ilişkisi. Saf ilişki tablosu olduğu için her iki FK
-- de ON DELETE CASCADE (bkz. docs/db-conventions.md).

CREATE TABLE follows (
    follower_actor_id  bigint NOT NULL REFERENCES actors (id) ON DELETE CASCADE,
    followed_actor_id  bigint NOT NULL REFERENCES actors (id) ON DELETE CASCADE,
    created_at         timestamptz NOT NULL DEFAULT now(),

    PRIMARY KEY (follower_actor_id, followed_actor_id),

    CONSTRAINT ck_follows_no_self CHECK (follower_actor_id <> followed_actor_id)
);

-- "Beni kimler takip ediyor" sorgusu için ters index (PK sadece
-- follower_actor_id ile başlayan sorguları hızlandırır, followed_actor_id
-- ile başlayanları değil).
CREATE INDEX idx_follows_followed_follower ON follows (followed_actor_id, follower_actor_id);

COMMENT ON TABLE follows IS
    'Actor''lar arası tek yönlü takip ilişkisi. PK (follower_actor_id, followed_actor_id) aynı '
    'çiftin iki kez eklenmesini engeller; ck_follows_no_self kimsenin kendini takip etmesine izin vermez.';
COMMENT ON COLUMN follows.follower_actor_id IS
    'Takip eden actor.';
COMMENT ON COLUMN follows.followed_actor_id IS
    'Takip edilen actor.';
