package database

import (
	"fmt"
	"time"

	"github.com/polaris/blog/internal/config"
	"github.com/polaris/blog/internal/models"
	"gorm.io/driver/mysql"
	"gorm.io/driver/postgres"
	"github.com/glebarez/sqlite"
	"gorm.io/gorm"
	gormlogger "gorm.io/gorm/logger"
)

// Connect connects to the database specified in cfg.
// For MySQL/PostgreSQL it first ensures the target database exists (auto-creates).
func Connect(cfg *config.Config) (*gorm.DB, error) {
	var dialector gorm.Dialector
	driver := cfg.Database.Driver

	gormConfig := &gorm.Config{
		Logger: gormlogger.Default.LogMode(gormlogger.Warn),
	}

	switch driver {
	case "mysql":
		// First connect without database to ensure it exists
		dsn := fmt.Sprintf("%s:%s@tcp(%s:%d)/?charset=utf8mb4&parseTime=True&loc=Local",
			cfg.Database.User, cfg.Database.Password,
			cfg.Database.Host, cfg.Database.Port,
		)
		bootstrapDB, err := gorm.Open(mysql.Open(dsn), gormConfig)
		if err != nil {
			return nil, fmt.Errorf("failed to connect MySQL server: %w", err)
		}
		dbName := cfg.Database.DBName
		if dbName == "" {
			dbName = "polaris"
		}
		bootstrapDB.Exec(fmt.Sprintf("CREATE DATABASE IF NOT EXISTS `%s` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci", dbName))
		sqlDB, _ := bootstrapDB.DB()
		if sqlDB != nil {
			sqlDB.Close()
		}
		// Reconnect with database selected
		dsn = fmt.Sprintf("%s:%s@tcp(%s:%d)/%s?charset=utf8mb4&parseTime=True&loc=Local",
			cfg.Database.User, cfg.Database.Password,
			cfg.Database.Host, cfg.Database.Port, dbName,
		)
		dialector = mysql.Open(dsn)

	case "postgres":
		sslMode := cfg.Database.SSLMode
		if sslMode == "" {
			sslMode = "disable"
		}
		dbName := cfg.Database.DBName
		if dbName == "" {
			dbName = "polaris"
		}
		// Connect to default 'postgres' db first to create target database
		dsn := fmt.Sprintf("host=%s port=%d user=%s password=%s dbname=postgres sslmode=%s",
			cfg.Database.Host, cfg.Database.Port,
			cfg.Database.User, cfg.Database.Password, sslMode,
		)
		bootstrapDB, err := gorm.Open(postgres.Open(dsn), gormConfig)
		if err != nil {
			// Fallback: try template1
			dsn = fmt.Sprintf("host=%s port=%d user=%s password=%s dbname=template1 sslmode=%s",
				cfg.Database.Host, cfg.Database.Port,
				cfg.Database.User, cfg.Database.Password, sslMode,
			)
			bootstrapDB, err = gorm.Open(postgres.Open(dsn), gormConfig)
			if err != nil {
				return nil, fmt.Errorf("failed to connect PostgreSQL server: %w", err)
			}
		}
		bootstrapDB.Exec(fmt.Sprintf("CREATE DATABASE \"%s\"", dbName))
		sqlDB, _ := bootstrapDB.DB()
		if sqlDB != nil {
			sqlDB.Close()
		}
		// Reconnect with target database
		dsn = fmt.Sprintf("host=%s port=%d user=%s password=%s dbname=%s sslmode=%s",
			cfg.Database.Host, cfg.Database.Port,
			cfg.Database.User, cfg.Database.Password, dbName, sslMode,
		)
		dialector = postgres.Open(dsn)

	default: // sqlite
		driver = "sqlite"
		path := cfg.Database.Path
		if path == "" {
			path = "polaris.db"
		}
		dialector = sqlite.Open(path)
	}

	db, err := gorm.Open(dialector, gormConfig)
	if err != nil {
		return nil, fmt.Errorf("failed to connect database (%s): %w", driver, err)
	}

	// Configure connection pool
	if sqlDB, err := db.DB(); err == nil {
		switch driver {
		case "mysql", "postgres":
			sqlDB.SetMaxIdleConns(10)
			sqlDB.SetMaxOpenConns(100)
			sqlDB.SetConnMaxLifetime(time.Hour)
		default: // sqlite
			sqlDB.SetMaxOpenConns(1) // SQLite only supports one writer
		}
	}

	// Verify connectivity with a ping
	sqlDB, err := db.DB()
	if err != nil {
		return nil, fmt.Errorf("failed to get underlying sql.DB: %w", err)
	}
	if err := sqlDB.Ping(); err != nil {
		return nil, fmt.Errorf("database ping failed: %w", err)
	}

	return db, nil
}

// Migrate runs AutoMigrate for all models.
func Migrate(db *gorm.DB) error {
	return db.AutoMigrate(
		&models.User{},
		&models.Post{},
		&models.Category{},
		&models.Tag{},
		&models.Comment{},
		&models.CommentLike{},
		&models.Media{},
		&models.PostVersion{},
		&models.FriendLink{},
		&models.Setting{},
		&models.Navigation{},
		&models.Page{},
		&models.Theme{},
		&models.Plugin{},
		&models.LoginAttempt{},
	)
}
