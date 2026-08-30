-- Genel `updated_at` bakım trigger'ı. `updated_at` kolonu olan her tabloya
-- bağlanır. Bu migration yazılırken information_schema.columns sorgusuyla
-- doğrulandı: bu kolona sahip tablolar sadece `actors` (0002) ve `votes`
-- (0009) -- ayrıntı için bu ajanın raporuna bakın.

CREATE FUNCTION set_updated_at()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, public
AS $$
BEGIN
    NEW.updated_at := now();
    RETURN NEW;
END;
$$;

CREATE TRIGGER trg_actors_set_updated_at
    BEFORE UPDATE ON actors
    FOR EACH ROW
    EXECUTE FUNCTION set_updated_at();

CREATE TRIGGER trg_votes_set_updated_at
    BEFORE UPDATE ON votes
    FOR EACH ROW
    EXECUTE FUNCTION set_updated_at();

COMMENT ON FUNCTION set_updated_at() IS
    'Genel BEFORE UPDATE trigger fonksiyonu: NEW.updated_at''i now()''a çeker. updated_at '
    'kolonu olan her tabloya bağlanır (şu an actors ve votes). Yeni bir tabloya updated_at '
    'eklenirse aynı fonksiyona bağlı yeni bir trg_<tablo>_set_updated_at trigger''ı eklenmelidir.';
