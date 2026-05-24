package app

import (
	"context"

	"github.com/polaris-blog/blog/internal/config"
	"github.com/polaris-blog/blog/internal/database"
	"github.com/polaris-blog/blog/internal/service"
)

func NewDatabase(cfg *config.Config) (*database.Database, error) {
	return database.New(cfg.Database)
}

func NeedsSetup(db *database.Database, cfg *config.Config) bool {
	authService := service.NewAuthService(db.Users, cfg.Security, nil)
	return !authService.HasAdmin(context.Background())
}
