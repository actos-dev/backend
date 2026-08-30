//! Actos'un domain katmanı: iş kuralları ve veritabanı erişimi.
//!
//! Buradaki hiçbir şey axum'u ya da HTTP'yi bilmez. Amaç, aynı mantığın
//! ileride farklı bir taşıma katmanından (gRPC, iş kuyruğu, seed script'i)
//! kullanılabilmesi ve testlerin HTTP kurmadan yazılabilmesi.
