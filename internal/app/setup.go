package app

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"html/template"
	"log/slog"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/polaris-blog/blog/internal/config"
	"github.com/polaris-blog/blog/internal/database"
	"github.com/polaris-blog/blog/internal/i18n"
	"github.com/polaris-blog/blog/internal/service"
	"gopkg.in/yaml.v3"
)

type SetupServer struct {
	paths        *ExtractedPaths
	templates    *template.Template
	i18nBundle   *i18n.Bundle
	logger       *slog.Logger
	mux          *http.ServeMux
	adminChecked bool
	adminExists  bool
	adminCheckMu sync.Mutex
}

func NewSetupServer(paths *ExtractedPaths, emb *EmbeddedFS, logger *slog.Logger) *SetupServer {
	s := &SetupServer{paths: paths, logger: logger}
	s.i18nBundle = i18n.NewBundle()
	if emb != nil {
		if err := s.i18nBundle.LoadFromFS(emb.Configs, "configs/locales"); err != nil {
			logger.Warn("i18n embed loading warning", slog.String("error", err.Error()))
		}
	}
	localesDir := filepath.Join(paths.ConfigsDir, "locales")
	if err := s.i18nBundle.LoadFromDir(localesDir); err != nil {
		logger.Debug("i18n disk loading note", slog.String("error", err.Error()))
	}
	s.i18nBundle.SetDefault("en")
	s.templates = s.loadTemplates()
	s.mux = http.NewServeMux()
	s.mux.HandleFunc("/", s.SetupPage)
	s.mux.HandleFunc("/api/setup/status", s.IsInstalled)
	s.mux.HandleFunc("/api/setup", s.Setup)
	s.mux.HandleFunc("/admin/static/", s.serveStatic)
	return s
}

func (s *SetupServer) loadTemplates() *template.Template {
	tmpl := template.New("").Funcs(template.FuncMap{"t": func(key string) string { return key }})
	dir := s.paths.AdminTemplates
	filepath.Walk(dir, func(path string, info os.FileInfo, err error) error {
		if err != nil || info == nil || info.IsDir() || !strings.HasSuffix(path, ".html") {
			return nil
		}
		rel, relErr := filepath.Rel(dir, path)
		if relErr != nil {
			return nil
		}
		if !strings.HasPrefix(rel, "setup") {
			return nil
		}
		data, readErr := os.ReadFile(path)
		if readErr != nil {
			return nil
		}
		if _, parseErr := tmpl.New(rel).Parse(string(data)); parseErr != nil {
			s.logger.Warn("parse template", slog.String("file", rel), slog.String("error", parseErr.Error()))
		}
		return nil
	})
	return tmpl
}

func (s *SetupServer) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	s.mux.ServeHTTP(w, r)
}

func (s *SetupServer) serveStatic(w http.ResponseWriter, r *http.Request) {
	staticDir := s.paths.AdminStatic
	fs := http.FileServer(http.Dir(staticDir))
	http.StripPrefix("/admin/static/", fs).ServeHTTP(w, r)
}

func (s *SetupServer) SetupPage(w http.ResponseWriter, r *http.Request) {
	if r.URL.Path != "/" && r.URL.Path != "/setup" {
		http.Redirect(w, r, "/setup", http.StatusSeeOther)
		return
	}
	lang := "en"
	if cookie, err := r.Cookie("polaris_lang"); err == nil && cookie.Value != "" {
		lang = cookie.Value
	}
	data := map[string]interface{}{
		"lang": lang,
	}
	tFunc := func(key string) string {
		return s.i18nBundle.T(lang, key)
	}
	if i18nJSON, err := s.i18nBundle.ToJSON(lang); err == nil {
		data["i18n_json"] = template.JS(string(i18nJSON))
	}
	tmpl := s.templates.Funcs(template.FuncMap{"t": tFunc})
	if err := tmpl.ExecuteTemplate(w, "setup.html", data); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
	}
}

func (s *SetupServer) IsInstalled(w http.ResponseWriter, r *http.Request) {
	installed := s.checkAdminExists()
	writeJSON(w, http.StatusOK, map[string]bool{"installed": installed})
}

type setupRequest struct {
	DBDriver        string `json:"db_driver"`
	DBDSN           string `json:"db_dsn"`
	CacheDriver     string `json:"cache_driver"`
	RedisAddr       string `json:"redis_addr"`
	RedisPassword   string `json:"redis_password"`
	RedisDB         int    `json:"redis_db"`
	SiteTitle       string `json:"site_title"`
	SiteDescription string `json:"site_description"`
	SiteURL         string `json:"site_url"`
	AdminUsername   string `json:"admin_username"`
	AdminEmail      string `json:"admin_email"`
	AdminPassword   string `json:"admin_password"`
}

type configYAML struct {
	Server   configServer   `yaml:"server"`
	Database configDatabase `yaml:"database"`
	Cache    configCache    `yaml:"cache"`
	Security configSecurity `yaml:"security"`
}

type configServer struct {
	Addr string `yaml:"addr"`
	Mode string `yaml:"mode"`
}

type configDatabase struct {
	Driver string `yaml:"driver"`
	DSN    string `yaml:"dsn"`
}

type configCache struct {
	Driver string      `yaml:"driver"`
	TTL    int         `yaml:"ttl"`
	Redis  configRedis `yaml:"redis"`
}

type configRedis struct {
	Addr     string `yaml:"addr"`
	Password string `yaml:"password,omitempty"`
	DB       int    `yaml:"db"`
}

type configSecurity struct {
	SecretKey   string `yaml:"secret_key"`
	SessionName string `yaml:"session_name"`
	CSRFEnabled bool   `yaml:"csrf_enabled"`
}

func (s *SetupServer) Setup(w http.ResponseWriter, r *http.Request) {
	if s.checkAdminExists() {
		writeJSON(w, http.StatusConflict, map[string]string{"error": "already installed"})
		return
	}

	var req setupRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		writeJSON(w, http.StatusBadRequest, map[string]string{"error": "invalid request"})
		return
	}

	if req.AdminUsername == "" || req.AdminEmail == "" || req.AdminPassword == "" {
		writeJSON(w, http.StatusBadRequest, map[string]string{"error": "all admin fields are required"})
		return
	}
	if len(req.AdminPassword) < 6 {
		writeJSON(w, http.StatusBadRequest, map[string]string{"error": "password must be at least 6 characters"})
		return
	}
	if req.DBDriver == "" {
		req.DBDriver = "sqlite"
	}
	if req.DBDSN == "" {
		req.DBDSN = "polaris.db"
	}

	secretKey := generateSecretKey()

	if err := s.writeConfig(req, secretKey); err != nil {
		s.logger.Error("write config", slog.String("error", err.Error()))
		writeJSON(w, http.StatusInternalServerError, map[string]string{"error": "failed to save configuration"})
		return
	}

	db, err := database.New(config.DatabaseConfig{Driver: req.DBDriver, DSN: req.DBDSN})
	if err != nil {
		s.logger.Error("database connection failed", slog.String("error", err.Error()))
		writeJSON(w, http.StatusInternalServerError, map[string]string{"error": "database connection failed"})
		return
	}
	defer db.Close()

	if err := db.Migrate(); err != nil {
		s.logger.Error("database migration failed", slog.String("error", err.Error()))
		writeJSON(w, http.StatusInternalServerError, map[string]string{"error": "database migration failed"})
		return
	}

	authService := service.NewAuthService(db.Users, config.SecurityConfig{
		SecretKey:   secretKey,
		SessionName: "polaris_session",
	}, nil)

	if _, err := authService.InitAdmin(r.Context(), req.AdminUsername, req.AdminEmail, req.AdminPassword); err != nil {
		s.logger.Error("init admin failed", slog.String("error", err.Error()))
		writeJSON(w, http.StatusInternalServerError, map[string]string{"error": "failed to create admin user"})
		return
	}

	if req.SiteTitle != "" {
		db.Options.Set(r.Context(), "site_title", req.SiteTitle)
	}
	if req.SiteDescription != "" {
		db.Options.Set(r.Context(), "site_description", req.SiteDescription)
	}
	if req.SiteURL != "" {
		db.Options.Set(r.Context(), "site_url", req.SiteURL)
	}

	writeJSON(w, http.StatusOK, map[string]string{"status": "ok", "restart": "true"})

	go func() {
		time.Sleep(1 * time.Second)
		RestartProcess()
	}()
}

func (s *SetupServer) writeConfig(req setupRequest, secretKey string) error {
	cacheDriver := req.CacheDriver
	if cacheDriver == "" {
		cacheDriver = "memory"
	}

	cfg := configYAML{
		Server: configServer{
			Addr: ":8080",
			Mode: "release",
		},
		Database: configDatabase{
			Driver: req.DBDriver,
			DSN:    req.DBDSN,
		},
		Cache: configCache{
			Driver: cacheDriver,
			TTL:    3600,
			Redis: configRedis{
				Addr:     req.RedisAddr,
				Password: req.RedisPassword,
				DB:       req.RedisDB,
			},
		},
		Security: configSecurity{
			SecretKey:   secretKey,
			SessionName: "polaris_session",
			CSRFEnabled: true,
		},
	}

	data, err := yaml.Marshal(cfg)
	if err != nil {
		return err
	}

	configPath := filepath.Join(s.paths.ConfigsDir, "default.yaml")
	return os.WriteFile(configPath, data, 0600)
}

func (s *SetupServer) checkAdminExists() bool {
	s.adminCheckMu.Lock()
	defer s.adminCheckMu.Unlock()
	if s.adminChecked {
		return s.adminExists
	}

	cfgPath := filepath.Join(s.paths.ConfigsDir, "default.yaml")
	cfg, err := config.Load(cfgPath)
	if err != nil {
		return false
	}

	db, err := database.New(cfg.Database)
	if err != nil {
		return false
	}
	defer db.Close()

	authService := service.NewAuthService(db.Users, cfg.Security, nil)
	s.adminExists = authService.HasAdmin(context.Background())
	s.adminChecked = true
	return s.adminExists
}

func generateSecretKey() string {
	b := make([]byte, 32)
	if _, err := rand.Read(b); err != nil {
		return hex.EncodeToString([]byte(time.Now().Format("20060102150405.000000000")))
	}
	return hex.EncodeToString(b)
}

func writeJSON(w http.ResponseWriter, status int, v interface{}) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	json.NewEncoder(w).Encode(v)
}

func RestartProcess() {
	executable, err := os.Executable()
	if err != nil {
		return
	}
	cmd := exec.Command(executable, os.Args[1:]...)
	cmd.Stdin = os.Stdin
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	if err := cmd.Start(); err != nil {
		return
	}
	os.Exit(0)
}
