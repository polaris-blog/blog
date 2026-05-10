package main

import (
	"log"
	"os"

	"github.com/polaris/blog/internal/config"
	"github.com/polaris/blog/internal/database"
	polaremb "github.com/polaris/blog/internal/embed"
	"github.com/polaris/blog/internal/server"
	"go.uber.org/zap"
)

func main() {
	cfg, err := config.Load()
	if err != nil {
		log.Fatalf("Failed to load config: %v", err)
	}

	logger, err := zap.NewProduction()
	if err != nil {
		log.Fatalf("Failed to create logger: %v", err)
	}
	defer logger.Sync()

	db, err := database.Connect(cfg)
	if err != nil {
		if cfg.Database.Driver == "sqlite" {
			logger.Fatal("Failed to connect SQLite. "+
				"If running in Docker with CGO_ENABLED=0, SQLite is unavailable. "+
				"Please use MySQL or PostgreSQL (set database.driver in config.yaml).",
				zap.Error(err))
		}
		logger.Fatal("Failed to connect to database", zap.Error(err))
	}

	if err := database.Migrate(db); err != nil {
		logger.Fatal("Failed to migrate database", zap.Error(err))
	}

	// Ensure directories exist
	os.MkdirAll("uploads", 0755)
	os.MkdirAll("plugins", 0755)
	os.MkdirAll("themes", 0755)

	// Release embedded assets (default theme + system templates)
	if err := polaremb.ReleaseDefaultTheme(".", logger); err != nil {
		logger.Warn("Failed to release default theme", zap.Error(err))
	}
	if err := polaremb.ReleaseSystemTemplates(".", logger); err != nil {
		logger.Warn("Failed to release system templates", zap.Error(err))
	}

	logger.Info("Starting Polaris blog server",
		zap.String("host", cfg.Server.Host),
		zap.Int("port", cfg.Server.Port),
	)

	srv := server.New(cfg, db, logger)
	if err := srv.Start(); err != nil {
		logger.Fatal("Failed to start server", zap.Error(err))
	}
}
