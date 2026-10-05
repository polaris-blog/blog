-- Search system v2 (MySQL 8+)
--
-- One denormalized index table for posts and pages plus a FULLTEXT index
-- over the searchable text columns. Matching/ranking uses MATCH ... AGAINST
-- (boolean mode for prefix matching, natural-language score for relevance).

CREATE TABLE IF NOT EXISTS search_index (
    id           BIGINT AUTO_INCREMENT PRIMARY KEY,
    ref_type     VARCHAR(8) NOT NULL,
    ref_id       BIGINT NOT NULL,
    title        VARCHAR(255) NOT NULL,
    slug         VARCHAR(191) NOT NULL,
    excerpt      TEXT NOT NULL,
    content      MEDIUMTEXT NOT NULL,
    author       VARCHAR(255) NOT NULL,
    category     VARCHAR(255) NOT NULL,
    tags         TEXT NOT NULL,
    visible      TINYINT NOT NULL DEFAULT 0,
    published_at BIGINT NULL,
    updated_at   BIGINT NOT NULL,
    UNIQUE KEY uq_search_ref (ref_type, ref_id),
    FULLTEXT KEY ft_search (title, excerpt, content, author, category, tags)
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4;

CREATE TABLE IF NOT EXISTS search_stats (
    query            VARCHAR(191) PRIMARY KEY,
    hits             BIGINT NOT NULL DEFAULT 0,
    no_results       BIGINT NOT NULL DEFAULT 0,
    last_searched_at BIGINT NOT NULL DEFAULT 0
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4;
