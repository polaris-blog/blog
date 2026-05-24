package app

import (
	"context"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"

	"github.com/polaris-blog/blog/internal/cache"
	"github.com/polaris-blog/blog/internal/config"
	"github.com/polaris-blog/blog/internal/database"
	"github.com/polaris-blog/blog/internal/i18n"
	httphandler "github.com/polaris-blog/blog/internal/http"
	"github.com/polaris-blog/blog/internal/plugin"
	"github.com/polaris-blog/blog/internal/service"
	"github.com/polaris-blog/blog/internal/theme"
)

type optionsSettingsStore struct {
	repo interface {
		Get(ctx context.Context, key string) (string, error)
		Set(ctx context.Context, key string, value string) error
	}
}

func (s *optionsSettingsStore) Get(key string) (string, error) {
	return s.repo.Get(context.Background(), key)
}

func (s *optionsSettingsStore) Set(key string, value string) error {
	return s.repo.Set(context.Background(), key, value)
}

type App struct {
	Config          *config.Config
	Database        *database.Database
	Cache           cache.Cache
	EventBus        *plugin.EventBus
	PluginManager   *plugin.Manager
	ThemeManager    *theme.Manager
	PostService     *service.PostService
	CommentService  *service.CommentService
	CategoryService *service.CategoryService
	TagService      *service.TagService
	AuthService     *service.AuthService
	Server          *httphandler.Server
	Logger          *slog.Logger
}

func New(cfg *config.Config, paths *ExtractedPaths, emb *EmbeddedFS) (*App, error) {
	logger := setupLogger(cfg.Log)

	db, err := database.New(cfg.Database)
	if err != nil {
		return nil, fmt.Errorf("init database: %w", err)
	}

	if err := db.Migrate(); err != nil {
		return nil, fmt.Errorf("run migrations: %w", err)
	}
	logger.Info("database connected and migrated", slog.String("driver", cfg.Database.Driver))

	appCache, err := cache.New(cfg.Cache)
	if err != nil {
		return nil, fmt.Errorf("init cache: %w", err)
	}
	logger.Info("cache initialized", slog.String("driver", cfg.Cache.Driver))

	eventBus := plugin.NewEventBus()
	pluginMgr := plugin.NewManager(eventBus)

	if err := pluginMgr.InitWasm(context.Background(), plugin.WasmConfig{
		MaxMemoryMB:    cfg.Plugin.Wasm.MaxMemoryMB,
		TimeoutSeconds: cfg.Plugin.Wasm.TimeoutSeconds,
	}); err != nil {
		logger.Warn("wasm runtime init warning", slog.String("error", err.Error()))
	}

	if paths != nil {
		cfg.Plugin.Dir = paths.PluginsDir
	}
	if cfg.Plugin.Dir != "" {
		pluginMgr.SetPluginDir(cfg.Plugin.Dir)
		if err := pluginMgr.LoadFromDir(context.Background()); err != nil {
			logger.Warn("plugin loading warning", slog.String("error", err.Error()))
		} else {
			plugins := pluginMgr.List()
			if len(plugins) > 0 {
				for _, p := range plugins {
					logger.Info("plugin loaded", slog.String("id", p.ID), slog.String("name", p.Name))
				}
			}
		}
	}

	debug := cfg.Server.Mode == "debug"
	themeDir := cfg.Theme.Dir
	if paths != nil {
		themeDir = paths.ThemesDir
	}
	themeMgr := theme.NewManager(themeDir, cfg.Theme.Active, debug)

	theme.SetFilterApplier(eventBus)

	optionsStore := &optionsSettingsStore{repo: db.Options}
	themeMgr.SetSettingsStore(optionsStore)
	pluginMgr.SetSettingsStore(optionsStore)

	for _, p := range pluginMgr.List() {
		pluginMgr.LoadPluginSettings(p.ID)
	}

	if err := themeMgr.LoadThemes(); err != nil {
		logger.Warn("theme loading warning", slog.String("error", err.Error()))
	}

	if activeID, err := db.Options.Get(context.Background(), "active_theme"); err == nil && activeID != "" {
		_ = themeMgr.SetActiveTheme(activeID)
	}

	for _, t := range themeMgr.ListThemes() {
		themeMgr.LoadThemeSettings(t.Meta.ID)
	}

	cacheTTL := cache.TTL(cfg.Cache)

	postService := service.NewPostService(db.Posts, db.Tags, eventBus, appCache, cacheTTL)
	commentService := service.NewCommentService(db.Comments, db.Posts, eventBus)
	categoryService := service.NewCategoryService(db.Categories, eventBus, appCache, cacheTTL)
	tagService := service.NewTagService(db.Tags, eventBus, appCache, cacheTTL)
	authService := service.NewAuthService(db.Users, cfg.Security, eventBus)

	if !authService.HasAdmin(context.Background()) {
		logger.Warn("no admin user found - please visit /setup to create one")
	}

	templateDir := "web/admin/templates"
	staticDir := "web/admin/static"
	configDir := "configs"
	uploadsDir := "./uploads"
	if paths != nil {
		templateDir = paths.AdminTemplates
		staticDir = paths.AdminStatic
		configDir = paths.ConfigsDir
		uploadsDir = filepath.Join(paths.DataDir, "uploads")
	}
	if err := os.MkdirAll(uploadsDir, 0755); err != nil {
		return nil, fmt.Errorf("create uploads dir: %w", err)
	}

	i18nBundle := i18n.NewBundle()
	if emb != nil {
		if err := i18nBundle.LoadFromFS(emb.Configs, "configs/locales"); err != nil {
			logger.Warn("i18n embed loading warning", slog.String("error", err.Error()))
		}
	}
	localesDir := filepath.Join(configDir, "locales")
	if err := i18nBundle.LoadFromDir(localesDir); err != nil {
		logger.Debug("i18n disk loading note", slog.String("error", err.Error()))
	}
	if len(i18nBundle.Languages()) == 0 {
		logger.Warn("no i18n locale files found")
	}
	i18nBundle.SetDefault("en")

	server := httphandler.NewServer(
		postService, commentService, authService,
		categoryService, tagService, themeMgr, pluginMgr,
		db.Options,
		templateDir, staticDir, configDir, uploadsDir,
		i18nBundle,
		logger,
	)

	app := &App{
		Config:          cfg,
		Database:        db,
		Cache:           appCache,
		EventBus:        eventBus,
		PluginManager:   pluginMgr,
		ThemeManager:    themeMgr,
		PostService:     postService,
		CommentService:  commentService,
		CategoryService: categoryService,
		TagService:      tagService,
		AuthService:     authService,
		Server:          server,
		Logger:          logger,
	}

	return app, nil
}

func (a *App) Run() error {
	return a.Server.ListenAndServe(a.Config.Server.Addr)
}

func (a *App) Shutdown(ctx context.Context) error {
	a.Logger.Info("shutting down application")

	if err := a.Server.Shutdown(ctx); err != nil {
		a.Logger.Error("shutdown http server", slog.String("error", err.Error()))
	}

	if a.PluginManager != nil {
		if err := a.PluginManager.Close(ctx); err != nil {
			a.Logger.Error("close plugin manager", slog.String("error", err.Error()))
		}
	}

	if a.Cache != nil {
		a.Cache.Close()
	}

	if err := a.Database.Close(); err != nil {
		a.Logger.Error("close database", slog.String("error", err.Error()))
	}

	a.Logger.Info("application stopped")
	return nil
}

func setupLogger(cfg config.LogConfig) *slog.Logger {
	var handler slog.Handler
	opts := &slog.HandlerOptions{Level: parseLogLevel(cfg.Level)}

	switch cfg.Format {
	case "text":
		handler = slog.NewTextHandler(os.Stdout, opts)
	default:
		handler = slog.NewJSONHandler(os.Stdout, opts)
	}

	return slog.New(handler)
}

func parseLogLevel(level string) slog.Level {
	switch level {
	case "debug":
		return slog.LevelDebug
	case "warn":
		return slog.LevelWarn
	case "error":
		return slog.LevelError
	default:
		return slog.LevelInfo
	}
}
