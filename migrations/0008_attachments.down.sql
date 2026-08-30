-- 0008_attachments.up.sql'i geri alır: index'leri ve tabloyu ters sırada düşürür.

DROP INDEX idx_attachments_orphaned;
DROP INDEX idx_attachments_content;
DROP TABLE attachments;
