//! Platformun **ilk** admin'ini oluşturan tek seferlik script.
//!
//! API üzerinden ilk admin oluşturulamaz — bunun ilk admin ataması bilinçli
//! bir güvenlik kararıdır: bu rolü kimin, ne zaman aldığı veritabanına
//! doğrudan erişimi olan biri tarafından, elle ve izlenebilir şekilde
//! belirlenir. Sonraki admin/moderatör atamaları normal API akışından
//! (mevcut bir admin'in `granted_by` alanıyla) yapılır.
//!
//! Kullanım:
//! ```text
//! cargo run -p actos-api --bin seed -- <kullanıcı_adı>
//! ```

use std::process::ExitCode;

use actos_core::{Config, db, secret};
use sqlx::PgPool;

#[tokio::main]
async fn main() -> ExitCode {
    // Geliştirmede .env; üretimde gerçek ortam değişkenleri kullanılır.
    let _ = dotenvy::dotenv();

    let username = match std::env::args().nth(1) {
        Some(u) if !u.trim().is_empty() => u,
        _ => {
            eprintln!("kullanım: cargo run -p actos-api --bin seed -- <kullanıcı_adı>");
            eprintln!(
                "örnek:    cargo run -p actos-api --bin seed -- efeadmin\n\
                 (kullanıcı adı actors.username kurallarına uymalı: küçük harf, 3-32 karakter)"
            );
            return ExitCode::FAILURE;
        }
    };

    match run(&username).await {
        Ok(Outcome::AdminCreated) => ExitCode::SUCCESS,
        Ok(Outcome::AdminAlreadyExists) => {
            // Bu bir hata değil, bir koruma: script'in yanlışlıkla ikinci
            // kez çalıştırılıp ikinci bir "ilk admin" yaratmasını
            // engelliyoruz. Yine de çağıranın devam etmemesi gerektiğini
            // net görebilmesi için başarısız çıkış koduyla dönüyoruz.
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("seed başarısız: {err}");
            ExitCode::FAILURE
        }
    }
}

enum Outcome {
    AdminCreated,
    AdminAlreadyExists,
}

async fn run(username: &str) -> Result<Outcome, Box<dyn std::error::Error>> {
    let config = Config::from_env()?;
    let pool = db::connect(&config.database).await?;

    if admin_already_exists(&pool).await? {
        println!(
            "Zaten bir admin var. Bu script yalnızca platformun İLK admin'ini oluşturmak \
             içindir ve bilerek tekrar çalıştırılamaz. Yeni admin/moderatör atamak için \
             mevcut bir admin'in API üzerinden rol vermesi gerekir."
        );
        return Ok(Outcome::AdminAlreadyExists);
    }

    let generated_key = secret::generate_api_key();
    let recovery_codes = secret::generate_recovery_codes(10)
        .map_err(|e| format!("kurtarma kodları üretilemedi: {e}"))?;

    let mut tx = pool.begin().await?;

    let actor_id = sqlx::query!(
        r#"
        INSERT INTO actors (username, actor_type)
        VALUES ($1, 'human')
        RETURNING id
        "#,
        username,
    )
    .fetch_one(&mut *tx)
    .await?
    .id;

    // granted_by = NULL: rolü veren başka bir admin yok, platformun ilk
    // admin'i bu script tarafından doğrudan atanıyor (bkz.
    // migrations/0012_admin_roles.up.sql üzerindeki COMMENT).
    sqlx::query!(
        r#"
        INSERT INTO admin_roles (actor_id, role, granted_by)
        VALUES ($1, 'admin', NULL)
        "#,
        actor_id,
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        r#"
        INSERT INTO api_keys (id, actor_id, secret_hash, label)
        VALUES ($1, $2, $3, $4)
        "#,
        generated_key.key_id,
        actor_id,
        generated_key.secret_hash,
        "ilk admin (seed script)",
    )
    .execute(&mut *tx)
    .await?;

    for code in &recovery_codes {
        sqlx::query!(
            r#"
            INSERT INTO recovery_codes (actor_id, code_hash)
            VALUES ($1, $2)
            "#,
            actor_id,
            code.hash,
        )
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    // Sırlar buradan sonra hiçbir yere (özellikle `tracing`'e) yazılmaz;
    // yalnızca stdout'a, yalnızca bu çalıştırmada basılır.
    print_secrets(
        username,
        actor_id,
        &generated_key.plaintext,
        &recovery_codes,
    );

    Ok(Outcome::AdminCreated)
}

async fn admin_already_exists(pool: &PgPool) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM admin_roles WHERE role = 'admin') AS "exists!""#
    )
    .fetch_one(pool)
    .await
}

fn print_secrets(
    username: &str,
    actor_id: i64,
    api_key: &str,
    recovery_codes: &[secret::GeneratedRecoveryCode],
) {
    println!();
    println!("========================================================================");
    println!(" İLK ADMIN OLUŞTURULDU");
    println!("========================================================================");
    println!(" kullanıcı adı : {username}");
    println!(" actor_id      : {actor_id}");
    println!();
    println!(" API KEY (bir daha gösterilmeyecek, şimdi güvenli bir yere kaydedin):");
    println!();
    println!("   {api_key}");
    println!();
    println!(" KURTARMA KODLARI (10 adet, her biri tek kullanımlık, bir daha gösterilmeyecek):");
    println!();
    for (i, code) in recovery_codes.iter().enumerate() {
        println!("   {:>2}. {}", i + 1, code.plaintext);
    }
    println!();
    println!("========================================================================");
}
