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

use actos_core::{
    Config,
    auth::{self, ActorType, AdminRole},
    db,
};
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

    // `auth::register` actor + ilk API key + 10 kurtarma kodunu tek
    // transaction'da yazar (bkz. crates/actos-core/src/auth.rs). Admin rolü
    // onun bilmediği ayrı bir kavram; bu yüzden `auth::grant_role` ikinci,
    // bağımsız bir adım. Aradaki kısacık pencerede bir hata olursa (actor
    // ve key var ama rol yok), bu script yeniden çalıştırılamaz
    // (`admin_already_exists` koruması yalnızca "bir admin var mı"na
    // bakar) — o durumda operatörün `admin_roles`'a satırı elle eklemesi
    // gerekir. Script tek seferlik ve elle çalıştırıldığı için bu
    // trade-off kabul edilebilir.
    let registration = auth::register(&pool, username, ActorType::Human, None).await?;

    // granted_by = None: rolü veren başka bir admin yok, platformun ilk
    // admin'i bu script tarafından doğrudan atanıyor (bkz.
    // migrations/0012_admin_roles.up.sql üzerindeki COMMENT).
    auth::grant_role(&pool, registration.actor.id, AdminRole::Admin, None).await?;

    // Sırlar buradan sonra hiçbir yere (özellikle `tracing`'e) yazılmaz;
    // yalnızca stdout'a, yalnızca bu çalıştırmada basılır.
    print_secrets(
        username,
        registration.actor.id,
        &registration.api_key,
        &registration.recovery_codes,
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

fn print_secrets(username: &str, actor_id: i64, api_key: &str, recovery_codes: &[String]) {
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
        println!("   {:>2}. {}", i + 1, code);
    }
    println!();
    println!("========================================================================");
}
