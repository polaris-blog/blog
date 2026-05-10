package config

import (
	"crypto/rand"
	"encoding/hex"
	"os"
	"time"

	"gopkg.in/yaml.v3"
)

// DBTestConfig is a minimal config used for installation database testing.
type DBTestConfig struct {
	Database DatabaseConfig `yaml:"database"`
}

type Config struct {
	Server   ServerConfig   `yaml:"server"`
	Database DatabaseConfig `yaml:"database"`
	Auth     AuthConfig     `yaml:"auth"`
	Cache    CacheConfig    `yaml:"cache"`
	Storage  StorageConfig  `yaml:"storage"`
}

type ServerConfig struct {
	Host            string        `yaml:"host"`
	Port            int           `yaml:"port"`
	ReadTimeout     time.Duration `yaml:"read_timeout"`
	WriteTimeout    time.Duration `yaml:"write_timeout"`
	ShutdownTimeout time.Duration `yaml:"shutdown_timeout"`
	CORSAllowOrigins string       `yaml:"cors_allow_origins"` // comma-separated, "*" for all (dev only)
}

type DatabaseConfig struct {
	Driver   string `yaml:"driver"`
	Host     string `yaml:"host"`
	Port     int    `yaml:"port"`
	User     string `yaml:"user"`
	Password string `yaml:"password"`
	DBName   string `yaml:"dbname"`
	SSLMode  string `yaml:"sslmode"`
	Path     string `yaml:"path"`
}

type AuthConfig struct {
	JWTSecret        string        `yaml:"jwt_secret"`
	TokenExpiry      time.Duration `yaml:"token_expiry"`
	MaxLoginAttempts int           `yaml:"max_login_attempts"`
	LockoutDuration  time.Duration `yaml:"lockout_duration"`
}

type CacheConfig struct {
	Enabled  bool          `yaml:"enabled"`
	Type     string        `yaml:"type"`
	Host     string        `yaml:"host"`
	Port     int           `yaml:"port"`
	Password string        `yaml:"password"`
	DB       int           `yaml:"db"`
	Expiry   time.Duration `yaml:"expiry"`
}

type StorageConfig struct {
	Type string `yaml:"type"`
	Path string `yaml:"path"`
}

func Load() (*Config, error) {
	configPath := os.Getenv("POLARIS_CONFIG")
	if configPath == "" {
		configPath = "config.yaml"
	}

	cfg := &Config{}

	data, err := os.ReadFile(configPath)
	if err != nil {
		// Config file not found — use defaults, no error
		_ = err
	} else if err := yaml.Unmarshal(data, cfg); err != nil {
		return nil, err
	}

	setDefaults(cfg)
	return cfg, nil
}

// generateRandomSecret creates a 32-byte random hex string for JWT signing.
func generateRandomSecret() string {
	b := make([]byte, 32)
	_, _ = rand.Read(b)
	return hex.EncodeToString(b)
}

func setDefaults(cfg *Config) {
	if cfg.Server.Host == "" {
		cfg.Server.Host = "0.0.0.0"
	}
	if cfg.Server.Port == 0 {
		cfg.Server.Port = 3000
	}
	if cfg.Server.ReadTimeout == 0 {
		cfg.Server.ReadTimeout = 10 * time.Second
	}
	if cfg.Server.WriteTimeout == 0 {
		cfg.Server.WriteTimeout = 10 * time.Second
	}
	if cfg.Server.ShutdownTimeout == 0 {
		cfg.Server.ShutdownTimeout = 5 * time.Second
	}

	if cfg.Database.Driver == "" {
		cfg.Database.Driver = "sqlite"
		cfg.Database.Path = "polaris.db"
	}

	if cfg.Auth.JWTSecret == "" {
		cfg.Auth.JWTSecret = generateRandomSecret()
	}
	if cfg.Auth.TokenExpiry == 0 {
		cfg.Auth.TokenExpiry = 7 * 24 * time.Hour
	}
	if cfg.Auth.MaxLoginAttempts == 0 {
		cfg.Auth.MaxLoginAttempts = 5
	}
	if cfg.Auth.LockoutDuration == 0 {
		cfg.Auth.LockoutDuration = 15 * time.Minute
	}

	if cfg.Cache.Expiry == 0 {
		cfg.Cache.Expiry = 10 * time.Minute
	}

	if cfg.Server.CORSAllowOrigins == "" {
		cfg.Server.CORSAllowOrigins = "*" // default: allow all (safe for dev, change in production)
	}

	if cfg.Storage.Type == "" {
		cfg.Storage.Type = "local"
		cfg.Storage.Path = "./uploads"
	}
}

// SaveInstallConfig writes the database configuration to config.yaml.
func SaveInstallConfig(db *DatabaseConfig) error {
	configPath := os.Getenv("POLARIS_CONFIG")
	if configPath == "" {
		configPath = "config.yaml"
	}

	cfg := DBTestConfig{
		Database: *db,
	}

	data, err := yaml.Marshal(&cfg)
	if err != nil {
		return err
	}

	return os.WriteFile(configPath, data, 0644)
}
