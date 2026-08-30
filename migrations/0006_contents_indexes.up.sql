-- contents tablosunun sorgu kalıplarına özel index'leri: alt ağaç sorguları,
-- yorum ağacı çekme, actor zaman çizelgesi ve üç feed sıralaması.

-- Alt ağaç sorguları için (`path <@ '<atalı path>'`): bir düğümün tüm
-- soyundan gelenlerini bulmak GIST üzerinden yapılır, ltree'nin native index
-- tipi budur.
CREATE INDEX idx_contents_path ON contents USING GIST (path);

-- Bir post'un tüm yorum ağacını (root_post_id = X) path sırasına göre
-- (yani ağaç gezinme sırasına göre) çekmek için.
CREATE INDEX idx_contents_root_path ON contents (root_post_id, path);

-- Bir actor'ün profilindeki canlı içeriklerini en yeniden en eskiye listelemek için.
CREATE INDEX idx_contents_actor_live ON contents (actor_id, created_at DESC)
    WHERE deleted_at IS NULL;

-- Feed sıralamaları: sadece canlı post'lar (yorumlar feed'de görünmez).
-- "hot" (varsayılan) sıralama.
CREATE INDEX idx_contents_hot ON contents (hot_score DESC, id DESC)
    WHERE content_type = 'post' AND deleted_at IS NULL;

-- "new" (en yeni) sıralama.
CREATE INDEX idx_contents_new ON contents (created_at DESC, id DESC)
    WHERE content_type = 'post' AND deleted_at IS NULL;

-- "top" (en yüksek skor) sıralama.
CREATE INDEX idx_contents_top ON contents (score DESC, id DESC)
    WHERE content_type = 'post' AND deleted_at IS NULL;

-- Bir içeriğin doğrudan çocuklarını (yanıtlarını) bulmak için.
CREATE INDEX idx_contents_parent ON contents (parent_content_id);
