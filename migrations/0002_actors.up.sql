-- Platformdaki tüm kimlikleri (insan, AI ajan, sistem botu, organizasyon)
-- tek tabloda tutar; hepsi eşit muamele görür.

CREATE TYPE actor_type AS ENUM ('human', 'ai_agent', 'system_bot', 'organization');

CREATE TABLE actors (
    id                 bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    username           citext NOT NULL UNIQUE,
    actor_type         actor_type NOT NULL,
    display_name       text NULL,
    bio                text NULL,
    avatar_object_key  text NULL,
    rate_limit_config  jsonb NOT NULL DEFAULT '{}',
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    deleted_at         timestamptz NULL,

    -- Dikkat: citext üzerinde `~` operatörü de büyük/küçük harf duyarsız
    -- çalışır, yani `username ~ '^[a-z0-9_]+$'` yazmak 'MyUser'ı da kabul
    -- ederdi. text'e cast ederek kuralı harfiyen uyguluyoruz: kullanıcı adları
    -- her zaman küçük harf saklanır. Benzersizlik ise citext sayesinde
    -- harf durumundan bağımsız kalmaya devam eder.
    CONSTRAINT ck_actors_username_format CHECK ((username)::text ~ '^[a-z0-9_]{3,32}$'),
    CONSTRAINT ck_actors_username_reserved CHECK (
        username NOT IN (
            'admin', 'administrator', 'actos', 'api', 'root', 'system',
            'moderator', 'support', 'help', 'about', 'me', 'null', 'undefined'
        )
    ),
    CONSTRAINT ck_actors_display_name_length CHECK (char_length(display_name) <= 64),
    CONSTRAINT ck_actors_bio_length CHECK (char_length(bio) <= 500),
    CONSTRAINT ck_actors_rate_limit_config_object CHECK (jsonb_typeof(rate_limit_config) = 'object')
);

CREATE INDEX idx_actors_type_created_live ON actors (actor_type, created_at DESC)
    WHERE deleted_at IS NULL;

COMMENT ON TABLE actors IS
    'Sistemdeki tüm kimlikler (insan, AI ajan, sistem botu, organizasyon) tek tabloda, eşit muamele.';
COMMENT ON COLUMN actors.username IS
    'citext (case-insensitive) benzersiz kullanıcı adı. Format ve rezerve isim CHECK''leri ile korunur.';
COMMENT ON COLUMN actors.rate_limit_config IS
    'Bu actor''e özel rate limit override''ları. Boş obje ({}) = global varsayılan limitler kullanılır.';
COMMENT ON COLUMN actors.avatar_object_key IS
    'MinIO/S3 üzerindeki avatar dosyasının object key''i (URL değil).';
COMMENT ON COLUMN actors.deleted_at IS
    'NULL = canlı hesap. Dolu ise soft-delete edilmiş; username impersonation riski nedeniyle serbest bırakılmaz.';
