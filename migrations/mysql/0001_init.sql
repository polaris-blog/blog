-- Polaris schema v1 (MySQL 8+)
CREATE TABLE IF NOT EXISTS settings (
    name  VARCHAR(191) PRIMARY KEY,
    value TEXT NOT NULL
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4;

CREATE TABLE IF NOT EXISTS users (
    id            BIGINT AUTO_INCREMENT PRIMARY KEY,
    username      VARCHAR(64) NOT NULL UNIQUE,
    email         VARCHAR(255) NOT NULL,
    password_hash VARCHAR(255) NOT NULL,
    role          VARCHAR(16) NOT NULL,
    display_name  VARCHAR(128) NOT NULL,
    bio           TEXT NOT NULL,
    created_at    BIGINT NOT NULL,
    updated_at    BIGINT NOT NULL
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4;

CREATE TABLE IF NOT EXISTS posts (
    id             BIGINT AUTO_INCREMENT PRIMARY KEY,
    title          VARCHAR(255) NOT NULL,
    slug           VARCHAR(191) NOT NULL UNIQUE,
    summary        TEXT NOT NULL,
    content_md     MEDIUMTEXT NOT NULL,
    author_id      BIGINT NOT NULL,
    status         VARCHAR(16) NOT NULL,
    featured_image VARCHAR(512) NULL,
    published_at   BIGINT NULL,
    created_at     BIGINT NOT NULL,
    updated_at     BIGINT NOT NULL,
    CONSTRAINT fk_posts_author FOREIGN KEY (author_id) REFERENCES users (id)
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4;
CREATE INDEX idx_posts_status_published ON posts (status, published_at);
CREATE INDEX idx_posts_author ON posts (author_id);
CREATE INDEX idx_posts_updated ON posts (updated_at);

CREATE TABLE IF NOT EXISTS pages (
    id         BIGINT AUTO_INCREMENT PRIMARY KEY,
    title      VARCHAR(255) NOT NULL,
    slug       VARCHAR(191) NOT NULL UNIQUE,
    summary    TEXT NOT NULL,
    content_md MEDIUMTEXT NOT NULL,
    author_id  BIGINT NOT NULL,
    status     VARCHAR(16) NOT NULL,
    sort_order BIGINT NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    CONSTRAINT fk_pages_author FOREIGN KEY (author_id) REFERENCES users (id)
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4;

CREATE TABLE IF NOT EXISTS terms (
    id   BIGINT AUTO_INCREMENT PRIMARY KEY,
    kind VARCHAR(16) NOT NULL,
    name VARCHAR(128) NOT NULL,
    slug VARCHAR(191) NOT NULL,
    UNIQUE KEY uq_terms_kind_slug (kind, slug)
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4;
CREATE INDEX idx_terms_kind ON terms (kind);

CREATE TABLE IF NOT EXISTS post_terms (
    post_id BIGINT NOT NULL,
    term_id BIGINT NOT NULL,
    PRIMARY KEY (post_id, term_id),
    CONSTRAINT fk_post_terms_post FOREIGN KEY (post_id) REFERENCES posts (id) ON DELETE CASCADE,
    CONSTRAINT fk_post_terms_term FOREIGN KEY (term_id) REFERENCES terms (id) ON DELETE CASCADE
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4;
CREATE INDEX idx_post_terms_term ON post_terms (term_id);

CREATE TABLE IF NOT EXISTS comments (
    id           BIGINT AUTO_INCREMENT PRIMARY KEY,
    post_id      BIGINT NOT NULL,
    parent_id    BIGINT NULL,
    author_name  VARCHAR(128) NOT NULL,
    author_email VARCHAR(255) NOT NULL,
    author_url   VARCHAR(512) NOT NULL,
    content      TEXT NOT NULL,
    status       VARCHAR(16) NOT NULL,
    created_at   BIGINT NOT NULL,
    CONSTRAINT fk_comments_post FOREIGN KEY (post_id) REFERENCES posts (id) ON DELETE CASCADE,
    CONSTRAINT fk_comments_parent FOREIGN KEY (parent_id) REFERENCES comments (id) ON DELETE CASCADE
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4;
CREATE INDEX idx_comments_post ON comments (post_id, status);
CREATE INDEX idx_comments_status ON comments (status);
