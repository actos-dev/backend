# syntax=docker/dockerfile:1.7
#
# Actos backend — çok aşamalı üretim imajı (PLAN.md Faz 19).
#
# Aşamalar:
#   chef    → ortak taban + cargo-chef
#   planner → bağımlılık reçetesi (recipe.json)
#   builder → önce yalnızca bağımlılıklar, sonra kaynak
#   runtime → yalnızca binary'ler
#
# `cargo chef cook` bağımlılıkları kaynak koddan **ayrı** bir katmanda
# derler: bir `.rs` dosyası değiştiğinde axum/sqlx/aws-sdk ağacı yeniden
# derlenmez. Bu ağaç temiz makinede ~10 dakika, cache ile ~1 dakika.

ARG RUST_VERSION=1.96

# ---------------------------------------------------------------- chef ---
FROM rust:${RUST_VERSION}-slim-bookworm AS chef
WORKDIR /app
# `ring` (rustls'in kripto sağlayıcısı) C kodu derliyor → cc gerekli.
# TLS için OpenSSL gerekmiyor: ağaç baştan sona rustls (bkz. Cargo.toml).
RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential pkg-config \
    && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --locked --version ^0.1

# ------------------------------------------------------------- planner ---
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ------------------------------------------------------------- builder ---
FROM chef AS builder

# `rust-toolchain.toml` kanalı sabitliyor; taban imajla aynı sürüm olduğu
# için rustup ek bir indirme yapmaz. Reçeteden önce kopyalanıyor ki
# toolchain çözümü de cache'lensin.
COPY rust-toolchain.toml ./

COPY --from=planner /app/recipe.json recipe.json
# Bağımlılık katmanı — kaynak kod HENÜZ kopyalanmadı, bilerek.
RUN cargo chef cook --release --recipe-path recipe.json

COPY . .

# `.git` build context'e girmiyor (bkz. .dockerignore), o yüzden commit
# SHA'sı dışarıdan veriliyor — `GET /version` bunu döndürüyor ve dağıtılmış
# bir sunucuda "hangi commit canlıda?" sorusunun tek cevabı bu.
ARG ACTOS_GIT_SHA=""
ENV ACTOS_GIT_SHA=${ACTOS_GIT_SHA}

# Derleme sırasında veritabanı yok: `sqlx` sorguları `.sqlx/` içindeki
# hazırlanmış meta veriden doğrular (offline mod). `.sqlx` bayatsa derleme
# burada kırılır — istenen davranış, sessizce çalışan bir sorgu değil.
ENV SQLX_OFFLINE=true

RUN cargo build --release --locked \
      --bin actos-api \
      --bin migrate \
      --bin seed \
    && strip target/release/actos-api target/release/migrate target/release/seed

# ------------------------------------------------------------- runtime ---
#
# `debian:bookworm-slim`, PLAN'daki "distroless/alpine" tercihinden bilinçli
# bir sapma. Gerekçe: docker-compose.prod.yml'ın `depends_on:
# condition: service_healthy` zinciri konteynerin **içinde** koşan bir
# healthcheck istiyor; distroless'ta ne kabuk ne curl var, o zaman sırf
# healthcheck için ayrı bir Rust binary'si yazmak gerekirdi. Ayrıca bu
# sunucuya SSH ile girilip hata ayıklanacak (bkz. SUNUCU.md) — kabuğu olan
# bir imaj orada gerçek bir kolaylık. Bedeli ~30 MB.
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin actos

COPY --from=builder /app/target/release/actos-api /usr/local/bin/actos-api
COPY --from=builder /app/target/release/migrate   /usr/local/bin/actos-migrate
COPY --from=builder /app/target/release/seed      /usr/local/bin/actos-seed

USER actos

# Konteyner içinde loopback'e değil, tüm arayüzlere bağlanmalı — aksi halde
# aynı ağdaki nginx/diğer servisler erişemez. Yayına açılma sınırı host
# tarafında (compose port bind'ı 127.0.0.1) çiziliyor.
ENV APP_HOST=0.0.0.0 \
    APP_PORT=3100 \
    RUST_LOG=actos_api=info,tower_http=info,sqlx=warn,info

EXPOSE 3100

# `/health` bağımlılıkları yoklamaz (o `/health/ready`); burada istenen
# "süreç ayakta ve HTTP'ye cevap veriyor mu". Hazır-olma sorgusu compose
# içinde ayrıca kullanılıyor.
HEALTHCHECK --interval=15s --timeout=5s --start-period=20s --retries=3 \
    CMD curl -fsS http://127.0.0.1:3100/health || exit 1

ENTRYPOINT ["/usr/local/bin/actos-api"]
