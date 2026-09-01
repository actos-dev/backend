//! İstemci IP'sinin çözümlenmesi: TCP soket adresi mi, `X-Forwarded-For`
//! header'ı mı.
//!
//! Bu ayrı, saf (`IpAddr` girer, `IpAddr` çıkar) bir fonksiyon olarak
//! tutuluyor ki hem birim testlerle kolayca doğrulanabilsin hem de
//! `crate::middleware::ratelimit`'in kendisi bu ayrıştırma detaylarıyla
//! şişmesin.
//!
//! ## Neden `X-Forwarded-For`'a körü körüne güvenilmiyor
//!
//! Bu header **istemci tarafından serbestçe ayarlanabilir** — aradan geçen
//! bir proxy yoksa (ya da proxy bu header'ı temizlemiyorsa), bir istemci
//! `X-Forwarded-For: 1.2.3.4` göndererek IP başına hız sınırını
//! doğrudan atlatabilir: her istekte farklı bir sahte IP göndermesi
//! yeterli. Bu yüzden varsayılan (`trusted_proxy_hops = 0`) bu header'ı
//! **tamamen yok sayar**; yalnızca operatör kaç güvenilir ters proxy
//! kurduğunu bilerek `trusted_proxy_hops`'u ayarladığında devreye girer.

use std::net::IpAddr;

/// `socket`: TCP bağlantısının gerçek karşı ucu (`ConnectInfo`) — her zaman
/// bilinir ve sahtelenemez, bu yüzden her durumda **güvenli varsayılan**.
///
/// `xff`: `X-Forwarded-For` header'ının ham değeri (varsa), virgülle
/// ayrılmış bir IP listesi.
///
/// `trusted_proxy_hops`: önümüzde kaç güvenilir ters proxy olduğu (bkz.
/// `actos_core::config::ServerConfig::trusted_proxy_hops` üzerindeki
/// yorum).
///
/// ## Seçim kuralı
///
/// - `trusted_proxy_hops == 0` → `xff` tamamen yok sayılır, `socket` döner.
/// - `trusted_proxy_hops == N > 0` → zincirin **sağdan `N+1`. girdisi**
///   alınır (soldan değil — sol taraf istemcinin uydurabildiği kısım).
/// - Zincir beklenenden **kısaysa** (`N+1` girdi yoksa), bozuk/eksik bir
///   `XFF`'e güvenip yanlış bir IP seçmek yerine güvenli tarafa (`socket`)
///   düşülür.
/// - Seçilen girdi geçerli bir IP olarak ayrıştırılamıyorsa (boş, bozuk
///   biçim) yine `socket`'e düşülür — bu fonksiyon asla panikleme/çökme
///   üretmez.
#[must_use]
pub fn resolve(socket: IpAddr, xff: Option<&str>, trusted_proxy_hops: usize) -> IpAddr {
    if trusted_proxy_hops == 0 {
        return socket;
    }

    let Some(xff) = xff else {
        return socket;
    };

    let entries: Vec<&str> = xff
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    // "sağdan N+1." girdi: 1-indexli "sağdan k." girdi, 0-indexli
    // (soldan) `len - k` konumuna denk gelir; k = N+1.
    let needed = trusted_proxy_hops + 1;
    if entries.len() < needed {
        return socket;
    }

    let idx = entries.len() - needed;
    entries[idx].parse().unwrap_or(socket)
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::resolve;

    fn ip(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    #[test]
    fn hops_sifirsa_xff_tamamen_yok_sayilir() {
        let socket = ip(203, 0, 113, 9);
        let result = resolve(socket, Some("9.9.9.9, 8.8.8.8"), 0);
        assert_eq!(result, socket, "hops=0 iken XFF'e hiç bakılmamalı");
    }

    #[test]
    fn hops_sifirsa_xff_hic_olmasa_da_sokete_duser() {
        let socket = ip(203, 0, 113, 9);
        assert_eq!(resolve(socket, None, 0), socket);
    }

    #[test]
    fn hops_bir_iken_sagdan_ikinci_girdi_seciliyor() {
        // 2 girdi, hops=1 → "sağdan 2." (N+1=2) girdi = soldan 1. = index 0.
        let socket = ip(10, 0, 0, 1);
        let result = resolve(socket, Some("5.5.5.5, 6.6.6.6"), 1);
        assert_eq!(
            result,
            ip(5, 5, 5, 5),
            "hops=1'de 2 girdilik zincirde sağdan 2. (index 0) seçilmeli"
        );
    }

    #[test]
    fn hops_iki_iken_sagdan_ucuncu_girdi_seciliyor() {
        // 3 girdi, hops=2 → sağdan 3. (N+1=3) girdi = soldan 1. = index 0.
        let socket = ip(10, 0, 0, 1);
        let result = resolve(socket, Some("1.1.1.1, 2.2.2.2, 3.3.3.3"), 2);
        assert_eq!(result, ip(1, 1, 1, 1));
    }

    #[test]
    fn hops_iki_iken_ekstra_soldaki_girdiler_atlanir() {
        // 4 girdi, hops=2 → sağdan 3. girdi = index (4-3)=1 = "2.2.2.2".
        // index 0 ("1.1.1.1") istemcinin uydurabildiği kısım, kullanılmamalı.
        let socket = ip(10, 0, 0, 1);
        let result = resolve(socket, Some("1.1.1.1, 2.2.2.2, 3.3.3.3, 4.4.4.4"), 2);
        assert_eq!(result, ip(2, 2, 2, 2));
    }

    #[test]
    fn zincir_beklenenden_kisaysa_sokete_duser() {
        // hops=2, gereken 3 girdi ama yalnızca 1 tanesi var.
        let socket = ip(10, 0, 0, 1);
        let result = resolve(socket, Some("9.9.9.9"), 2);
        assert_eq!(
            result, socket,
            "kısa zincirde güvenli tarafa (soket) düşülmeli"
        );
    }

    #[test]
    fn bos_xff_cokertmiyor_sokete_duser() {
        let socket = ip(10, 0, 0, 1);
        assert_eq!(resolve(socket, Some(""), 1), socket);
        assert_eq!(resolve(socket, Some("   "), 1), socket);
        assert_eq!(resolve(socket, Some(",,,"), 1), socket);
    }

    #[test]
    fn bozuk_ip_cokertmiyor_sokete_duser() {
        let socket = ip(10, 0, 0, 1);
        // 2 girdi, hops=1 → index 0 = "bozuk-bir-deger" → parse başarısız.
        let result = resolve(socket, Some("bozuk-bir-deger, 6.6.6.6"), 1);
        assert_eq!(result, socket);
    }

    #[test]
    fn fazla_bosluklu_girdiler_dogru_ayristiriliyor() {
        let socket = ip(10, 0, 0, 1);
        let result = resolve(socket, Some("  7.7.7.7  ,  8.8.8.8  "), 1);
        assert_eq!(result, ip(7, 7, 7, 7));
    }
}
