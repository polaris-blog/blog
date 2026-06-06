package database

import (
	"fmt"
	"time"

	"github.com/glebarez/sqlite"
	"github.com/polaris-blog/blog/internal/config"
	"github.com/polaris-blog/blog/internal/model"
	"github.com/polaris-blog/blog/internal/repository"
	gormrepo "github.com/polaris-blog/blog/internal/repository/gorm"
	"gorm.io/driver/mysql"
	"gorm.io/driver/postgres"
	"gorm.io/gorm"
	"gorm.io/gorm/logger"
)

type Database struct {
	db         *gorm.DB
	Posts      repository.PostRepository
	Users      repository.UserRepository
	Categories repository.CategoryRepository
	Tags       repository.TagRepository
	Comments   repository.CommentRepository
	Media      repository.MediaRepository
	Options    repository.OptionRepository
}

func New(cfg config.DatabaseConfig) (*Database, error) {
	var dialector gorm.Dialector

	switch cfg.Driver {
	case "sqlite":
		dialector = sqlite.Open(cfg.DSN)
	case "mysql":
		dialector = mysql.Open(cfg.DSN)
	case "postgres":
		dialector = postgres.Open(cfg.DSN)
	default:
		return nil, fmt.Errorf("unsupported database driver: %s", cfg.Driver)
	}

	gormConfig := &gorm.Config{
		Logger: logger.Default.LogMode(logger.Silent),
	}

	db, err := gorm.Open(dialector, gormConfig)
	if err != nil {
		return nil, fmt.Errorf("open database: %w", err)
	}

	if cfg.Driver == "sqlite" {
		db.Exec("PRAGMA journal_mode=WAL")
		db.Exec("PRAGMA busy_timeout=5000")
		db.Exec("PRAGMA synchronous=NORMAL")
		db.Exec("PRAGMA cache_size=-64000")
		db.Exec("PRAGMA temp_store=MEMORY")
	} else {
		sqlDB, _ := db.DB()
		if sqlDB != nil {
			sqlDB.SetMaxOpenConns(25)
			sqlDB.SetMaxIdleConns(10)
			sqlDB.SetConnMaxLifetime(5 * time.Minute)
			sqlDB.SetConnMaxIdleTime(10 * time.Minute)
		}
	}

	d := &Database{db: db}
	d.Posts = gormrepo.NewPostRepo(db)
	d.Users = gormrepo.NewUserRepo(db)
	d.Categories = gormrepo.NewCategoryRepo(db)
	d.Tags = gormrepo.NewTagRepo(db)
	d.Comments = gormrepo.NewCommentRepo(db)
	d.Media = gormrepo.NewMediaRepo(db)
	d.Options = gormrepo.NewOptionRepo(db)

	return d, nil
}

func (d *Database) Close() error {
	sqlDB, err := d.db.DB()
	if err != nil {
		return err
	}
	return sqlDB.Close()
}

func (d *Database) Migrate() error {
	if err := d.db.AutoMigrate(
		&model.User{},
		&model.Category{},
		&model.Tag{},
		&model.Post{},
		&model.PostMeta{},
		&model.Comment{},
		&model.Media{},
		&model.Option{},
	); err != nil {
		return err
	}

	m := d.db.Migrator()
	if !m.HasIndex(&model.Post{}, "idx_posts_slug") {
		d.db.Exec("CREATE UNIQUE INDEX idx_posts_slug ON posts(slug)")
	}
	if !m.HasIndex(&model.Post{}, "idx_posts_status") {
		d.db.Exec("CREATE INDEX idx_posts_status ON posts(status)")
	}
	if !m.HasIndex(&model.Post{}, "idx_posts_type_status") {
		d.db.Exec("CREATE INDEX idx_posts_type_status ON posts(type, status)")
	}
	if !m.HasIndex(&model.Post{}, "idx_posts_created_at") {
		d.db.Exec("CREATE INDEX idx_posts_created_at ON posts(created_at DESC)")
	}
	if !m.HasIndex(&model.Comment{}, "idx_comments_status") {
		d.db.Exec("CREATE INDEX idx_comments_status ON comments(status)")
	}
	if !m.HasIndex(&model.Comment{}, "idx_comments_post_id") {
		d.db.Exec("CREATE INDEX idx_comments_post_id ON comments(post_id)")
	}
	return nil
}
