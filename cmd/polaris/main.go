package main

import (
	"context"
	"fmt"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"syscall"
	"time"

	"github.com/polaris-blog/blog"
	"github.com/polaris-blog/blog/internal/app"
	"github.com/polaris-blog/blog/internal/config"
)

func main() {
	dataDir := ".polaris"
	if len(os.Args) > 1 {
		if os.Args[1] == "-h" || os.Args[1] == "--help" {
			fmt.Println("Usage: polaris [data-dir] [config-path]")
			fmt.Println()
			fmt.Println("  data-dir    Directory for extracted files (default: .polaris)")
			fmt.Println("  config-path Path to config file (default: <data-dir>/configs/default.yaml)")
			os.Exit(0)
		}
		dataDir = os.Args[1]
	}

	logger := slog.Default()

	emb := &app.EmbeddedFS{
		AdminTemplates: blog.AdminTemplates,
		AdminStatic:    blog.AdminStatic,
		DefaultTheme:   blog.DefaultTheme,
		Configs:        blog.Configs,
		Plugins:        blog.Plugins,
	}

	paths, err := app.SelfExtract(dataDir, emb, logger)
	if err != nil {
		slog.Error("self-extract failed", slog.String("error", err.Error()))
		os.Exit(1)
	}

	cfgPath := filepath.Join(paths.ConfigsDir, "default.yaml")
	if len(os.Args) > 2 {
		cfgPath = os.Args[2]
	}

	cfg, err := config.Load(cfgPath)
	if err != nil {
		slog.Error("load config", slog.String("error", err.Error()))
		os.Exit(1)
	}

	if needsSetup(cfg, paths, logger) {
		runSetupMode(cfg, paths, emb, logger)
		return
	}

	runNormalMode(cfg, paths, emb, logger)
}

func needsSetup(cfg *config.Config, paths *app.ExtractedPaths, logger *slog.Logger) bool {
	db, err := app.NewDatabase(cfg)
	if err != nil {
		logger.Info("database not available, entering setup mode", slog.String("error", err.Error()))
		return true
	}
	defer db.Close()

	if err := db.Migrate(); err != nil {
		logger.Info("database migration failed, entering setup mode", slog.String("error", err.Error()))
		return true
	}

	return app.NeedsSetup(db, cfg)
}

func runSetupMode(cfg *config.Config, paths *app.ExtractedPaths, emb *app.EmbeddedFS, logger *slog.Logger) {
	setupServer := app.NewSetupServer(paths, emb, logger)

	addr := cfg.Server.Addr
	if addr == "" {
		addr = ":8080"
	}

	logger.Info(fmt.Sprintf("setup mode - please visit http://localhost%s to complete installation", addr))

	httpServer := &http.Server{
		Addr:           addr,
		Handler:        setupServer,
		ReadTimeout:    15 * time.Second,
		WriteTimeout:   30 * time.Second,
		IdleTimeout:    120 * time.Second,
		MaxHeaderBytes: 1 << 20,
	}

	go func() {
		if err := httpServer.ListenAndServe(); err != nil && err != http.ErrServerClosed {
			logger.Error("setup server error", slog.String("error", err.Error()))
			os.Exit(1)
		}
	}()

	quit := make(chan os.Signal, 1)
	signal.Notify(quit, syscall.SIGINT, syscall.SIGTERM)
	<-quit

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	httpServer.Shutdown(ctx)
}

func runNormalMode(cfg *config.Config, paths *app.ExtractedPaths, emb *app.EmbeddedFS, logger *slog.Logger) {
	application, err := app.New(cfg, paths, emb)
	if err != nil {
		slog.Error("init app", slog.String("error", err.Error()))
		os.Exit(1)
	}

	go func() {
		if err := application.Run(); err != nil {
			application.Logger.Error("server error", slog.String("error", err.Error()))
			os.Exit(1)
		}
	}()

	application.Logger.Info(fmt.Sprintf("polaris is running on %s", cfg.Server.Addr))

	quit := make(chan os.Signal, 1)
	signal.Notify(quit, syscall.SIGINT, syscall.SIGTERM)
	<-quit

	application.Logger.Info("shutting down...")

	shutdownCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	if err := application.Shutdown(shutdownCtx); err != nil {
		application.Logger.Error("shutdown error", slog.String("error", err.Error()))
	}
}
