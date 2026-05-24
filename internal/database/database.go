package database

import (
	"fmt"

	"github.com/polaris-blog/blog/internal/config"
	"github.com/polaris-blog/blog/internal/model"
	"github.com/polaris-blog/blog/internal/repository"
	gormrepo "github.com/polaris-blog/blog/internal/repository/gorm"
	"gorm.io/driver/mysql"
	"gorm.io/driver/postgres"
	"gorm.io/driver/sqlite"
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

func (d *Database) DB() *gorm.DB {
	return d.db
}

func (d *Database) Close() error {
	sqlDB, err := d.db.DB()
	if err != nil {
		return err
	}
	return sqlDB.Close()
}

func (d *Database) Ping() error {
	sqlDB, err := d.db.DB()
	if err != nil {
		return err
	}
	return sqlDB.Ping()
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
	if m.HasIndex(&model.Post{}, "idx_posts_slug") {
		m.DropIndex(&model.Post{}, "idx_posts_slug")
	}
	if !m.HasIndex(&model.Post{}, "idx_posts_slug") {
		d.db.Exec("CREATE UNIQUE INDEX idx_posts_slug ON posts(slug)")
	}
	return nil
}
