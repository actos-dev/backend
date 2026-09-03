-- Bildirimler (v1'e alındı, bkz. NOTES.md §1 ve PLAN.md Faz 18.A). Amaç: bir
-- actor'ün ("postuma yanıt geldi mi?") sorusunu N yoklama isteği yerine TEK
-- bir `GET /me/inbox` isteğiyle yanıtlayabilmesi -- yazma anındaki tek satır
-- ekleme, N sorgunun tamamının önüne geçiyor. Detaylı analiz ve alternatif
-- (webhook) NOTES.md §1'de: webhook teslim garantisi/yeniden deneme/imzalama
-- gerektiren daha büyük bir iş, inbox onun %80'ini %10 maliyetle veriyor.

CREATE TYPE notification_kind AS ENUM (
    'comment_on_post',
    'reply_to_comment',
    'new_follower',
    'moderation_action'
);
-- `direct_message` BİLEREK burada yok. DM'in kendisi v1 kapsamı dışında
-- (NOTES.md §5, "DM + inbox: baştan bilinmesi gereken tasarım kısıtı") ve
-- tasarımı (uçtan uca şifreleme, anahtar değişimi) henüz kesinleşmedi. Ama
-- şema bunu ENGELLEMİYOR: `ALTER TYPE notification_kind ADD VALUE
-- 'direct_message'` DM işi başladığında tek satırlık, geriye dönük uyumlu
-- bir migration olarak eklenebilir -- bu tabloya ya da `kind` sütununa hiç
-- dokunmadan. (PostgreSQL 12+ `ADD VALUE`'yu aynı transaction içinde COMMIT
-- olmadan da kullanılabilir kılıyor; bu depo Postgres 18 hedefliyor, sorun
-- yok.)

CREATE TABLE notifications (
    id                  bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    recipient_actor_id  bigint NOT NULL REFERENCES actors (id) ON DELETE RESTRICT,
    kind                notification_kind NOT NULL,
    actor_id            bigint NULL REFERENCES actors (id) ON DELETE RESTRICT,
    target_type         text NOT NULL,
    target_id           bigint NOT NULL,
    payload             jsonb NOT NULL DEFAULT '{}',
    created_at          timestamptz NOT NULL DEFAULT now(),
    read_at             timestamptz NULL,

    CONSTRAINT ck_notifications_target_type_length CHECK (char_length(target_type) BETWEEN 1 AND 32),
    CONSTRAINT ck_notifications_payload_object CHECK (jsonb_typeof(payload) = 'object')
);

-- `GET /me/inbox`'ın ana erişim yolu: bir actor'ün bildirimleri en yeniden
-- eskiye, keyset (cursor) sayfalamayla (bkz. `actos_core::cursor` -- bu
-- tabloda YENİ bir sayfalama icat edilmiyor, `SortKey::New` aynen kullanılıyor).
CREATE INDEX idx_notifications_recipient_created ON notifications (recipient_actor_id, created_at DESC);

-- Okunmamış sayımı (`unread_count`, `GET /me/inbox` yanıtının bir parçası)
-- ve `?unread=true` filtresi için ayrı, kısmi bir index: `read_at IS NULL`
-- satırları toplamda küçük bir azınlık olacağı için (bir bildirim okunduktan
-- sonra sonsuza dek bu index'in dışında kalır), tam tabloyu tarayan bir
-- `COUNT(*) WHERE read_at IS NULL` yerine bu index'in kendisinin boyutu
-- "aktif" bildirim sayısıyla orantılı kalır.
CREATE INDEX idx_notifications_recipient_unread ON notifications (recipient_actor_id) WHERE read_at IS NULL;

COMMENT ON TABLE notifications IS
    'Bir actor''e "sana bir şey oldu" diyen bildirim satırları (bkz. NOTES.md §1). Yazma anında, '
    'tetikleyen eylemle AYNI transaction''da eklenir (bkz. crate::comment::create_comment, '
    'crate::interaction::follow, crate::moderation) -- sessizce kaybolmaması için. `GET /me/inbox` '
    'bunu keyset cursor''la okur. Hedefi silinen (soft-delete) bir bildirim SATIR OLARAK KALIR -- '
    'geçmiş kaybolmasın diye -- istemci hedefi ayrıca çekince oradan 410 alır, notifications '
    'tablosunun kendisi bunu maskelemez/temizlemez.';
COMMENT ON COLUMN notifications.recipient_actor_id IS
    'Bildirimi görecek actor. RESTRICT: bkz. migrations/0018_fix_actor_fk_delete_rules.up.sql -- '
    'actor''lar normalde soft-delete edilir, bir actor satırının hard-delete''i (istisnai, ör. '
    'yasal silme talebi) önce bu bildirimlerin bilinçli olarak temizlenmesini gerektirmeli.';
COMMENT ON COLUMN notifications.kind IS
    'Bildirim türü (bkz. notification_kind). comment_on_post: kök postunun yazarına, yeni bir '
    'yorum geldiğinde. reply_to_comment: bir yorumun DOĞRUDAN yazarına, o yoruma yanıt '
    'geldiğinde -- ataların TAMAMINA değil (bkz. crate::comment::create_comment üzerindeki '
    '"fan-out sınırı" yorumu: 32 seviyelik bir dalda tek bir yorum yalnızca 2 satır üretir, '
    '32 değil). new_follower: yeni bir takipçi edinildiğinde. moderation_action: bir moderatör '
    'eylemi (içerik silme, ban) doğrudan o eylemin hedefine.';
COMMENT ON COLUMN notifications.actor_id IS
    'Bildirimi TETİKLEYEN actor (yorumu yazan, takip eden, moderasyon eylemini yapan admin). '
    'NULL yalnızca insan/actor kaynaklı olmayan sistem olaylarında kullanılmak üzere şemada '
    'bilerek nullable bırakıldı -- bugün üreten hiçbir yol yok, ama ileride ör. periyodik bir '
    'işin ürettiği bir bildirim (bir "actor" olmayan bir tetikleyici) bu sütuna dokunmadan '
    'eklenebilsin diye. recipient_actor_id ile AYNI olamaz diye bir kısıt YOK şema seviyesinde -- '
    'bu, "kendi eylemin sana bildirim üretmez" kuralının uygulama katmanında (bkz. '
    'crate::notification::create_notification) tutulduğu, burada tekrarlanmadığı anlamına gelir; '
    'iki katmanlı savunma (crate::comment::create_comment gibi) burada gerekmiyor çünkü ihlalin '
    'bedeli bir kullanıcı hatası değil, sadece gereksiz bir satır -- veri bütünlüğünü bozmuyor.';
COMMENT ON COLUMN notifications.target_type IS
    'Hedefin türü, serbest metin (ör. "content", "actor"). migrations/0015_admin_actions_log.up.sql''deki '
    'admin_actions_log.target_type ile AYNI desen ve AYNI gerekçe: hedef bazen bir content (yeni '
    'yorum), bazen bir actor (yeni takipçi) olabiliyor -- tek bir FK ikisini birden karşılayamaz, '
    'bilerek FK DEĞİL. Ayrıca ileride direct_message eklendiğinde yeni bir target_type (ör. '
    '"message") gerekebilir; text olması bunun için ayrı bir migration/enum genişletmesi gerektirmez.';
COMMENT ON COLUMN notifications.target_id IS
    'target_type''e göre yorumlanan id (contents.id ya da actors.id). Hedef sonradan soft-delete '
    'edilirse bu satır SİLİNMEZ (bkz. tablo yorumu) -- istemci hedefi GET ile çekmeye çalışınca '
    'oradan 410 alır, bildirim geçmişte "gerçekten olmuş" bir olayın kaydı olarak kalmaya devam eder.';
COMMENT ON COLUMN notifications.payload IS
    'Tür (kind) başına OPSİYONEL ek veri, boş obje (''{}'') varsayılan. BİLEREK ZORUNLU bir '
    '"preview" kolonu YOK (bkz. NOTES.md §5, "DM + inbox" bölümü): DM uçtan uca şifreli '
    'hedeflendiği için sunucu düz metni hiç göremeyecek; bugün "kolaylık olsun" diye şemaya '
    'zorunlu bir önizleme alanı eklemek, yarın DM''i ya imkânsız kılar ya da tüm istemcileri '
    'kıran bir kaldırma gerektirir. Bunun yerine her kind kendi opsiyonel alanlarını bu jsonb''nin '
    'içinde taşır (ör. moderation_action: {"action_type": "...", "reason": "..."}) -- şema '
    'seviyesinde hiçbir kind''e hiçbir alan ZORUNLU değildir.';
COMMENT ON COLUMN notifications.read_at IS
    'NULL = okunmadı. `PATCH /me/inbox/{id}/read` (tekil) ve `POST /me/inbox/read` (toplu, "şu '
    'cursor''a kadar hepsi") ikisi de idempotent: zaten okunmuş bir satıra tekrar dokunmak '
    'read_at''i İLERİ ATMAZ (COALESCE(read_at, now())).';
