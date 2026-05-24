package config

import (
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"os"

	"gopkg.in/yaml.v3"
)

type Config struct {
	Server   ServerConfig   `yaml:"server"`
	Database DatabaseConfig `yaml:"database"`
	Cache    CacheConfig    `yaml:"cache"`
	Storage  StorageConfig  `yaml:"storage"`
	Theme    ThemeConfig    `yaml:"theme"`
	Plugin   PluginConfig   `yaml:"plugin"`
	Search   SearchConfig   `yaml:"search"`
	Log      LogConfig      `yaml:"log"`
	Security SecurityConfig `yaml:"security"`
}

type ServerConfig struct {
	Addr string `yaml:"addr"`
	Mode string `yaml:"mode"`
}

type DatabaseConfig struct {
	Driver string `yaml:"driver"`
	DSN    string `yaml:"dsn"`
}

type CacheConfig struct {
	Driver string     `yaml:"driver"`
	TTL    int        `yaml:"ttl"`
	Redis  RedisConfig `yaml:"redis"`
}

type RedisConfig struct {
	Addr     string `yaml:"addr"`
	Password string `yaml:"password"`
	DB       int    `yaml:"db"`
}

type StorageConfig struct {
	Driver string       `yaml:"driver"`
	Local  LocalStorage `yaml:"local"`
	S3     S3Storage    `yaml:"s3"`
}

type LocalStorage struct {
	Path string `yaml:"path"`
}

type S3Storage struct {
	Bucket   string `yaml:"bucket"`
	Region   string `yaml:"region"`
	Endpoint string `yaml:"endpoint"`
	AK       string `yaml:"access_key"`
	SK       string `yaml:"secret_key"`
}

type ThemeConfig struct {
	Active string `yaml:"active"`
	Dir    string `yaml:"dir"`
}

type PluginConfig struct {
	Dir     string      `yaml:"dir"`
	Enabled []string    `yaml:"enabled"`
	Wasm    WasmConfig  `yaml:"wasm"`
}

type WasmConfig struct {
	MaxMemoryMB    int  `yaml:"max_memory_mb"`
	TimeoutSeconds int  `yaml:"timeout_seconds"`
}

type SearchConfig struct {
	Driver       string           `yaml:"driver"`
	Meilisearch  MeilisearchConfig `yaml:"meilisearch"`
}

type MeilisearchConfig struct {
	Addr   string `yaml:"addr"`
	APIKey string `yaml:"api_key"`
}

type LogConfig struct {
	Level  string `yaml:"level"`
	Format string `yaml:"format"`
}

type SecurityConfig struct {
	SecretKey    string `yaml:"secret_key"`
	SessionName  string `yaml:"session_name"`
	CSRFEnabled  bool   `yaml:"csrf_enabled"`
}

func Default() *Config {
	return &Config{
		Server: ServerConfig{
			Addr: ":8080",
			Mode: "release",
		},
		Database: DatabaseConfig{
			Driver: "sqlite",
			DSN:    "polaris.db",
		},
		Cache: CacheConfig{
			Driver: "memory",
			TTL:    3600,
		},
		Storage: StorageConfig{
			Driver: "local",
			Local: LocalStorage{
				Path: "./uploads",
			},
		},
		Theme: ThemeConfig{
			Active: "default",
			Dir:    "./themes",
		},
		Plugin: PluginConfig{
			Dir:     "./plugins",
			Enabled: []string{},
		},
		Search: SearchConfig{
			Driver: "bleve",
		},
		Log: LogConfig{
			Level:  "info",
			Format: "json",
		},
		Security: SecurityConfig{
			SecretKey:   "polaris-secret-change-me",
			SessionName: "polaris_session",
			CSRFEnabled: true,
		},
	}
}

func Load(path string) (*Config, error) {
	cfg := Default()

	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return cfg, nil
		}
		return nil, fmt.Errorf("read config file: %w", err)
	}

	if err := yaml.Unmarshal(data, cfg); err != nil {
		return nil, fmt.Errorf("parse config file: %w", err)
	}

	cfg.applyEnvOverrides()

	if cfg.Security.SecretKey == "polaris-secret-change-me" {
		b := make([]byte, 32)
		if _, err := rand.Read(b); err == nil {
			cfg.Security.SecretKey = hex.EncodeToString(b)
		}
		fmt.Fprintf(os.Stderr, "WARNING: using default secret key is insecure. Set POLARIS_SECRET_KEY environment variable or configure security.secret_key in config file.\n")
	}

	return cfg, nil
}

func (c *Config) applyEnvOverrides() {
	if v := os.Getenv("POLARIS_ADDR"); v != "" {
		c.Server.Addr = v
	}
	if v := os.Getenv("POLARIS_DB_DRIVER"); v != "" {
		c.Database.Driver = v
	}
	if v := os.Getenv("POLARIS_DB_DSN"); v != "" {
		c.Database.DSN = v
	}
	if v := os.Getenv("POLARIS_SECRET_KEY"); v != "" {
		c.Security.SecretKey = v
	}
	if v := os.Getenv("POLARIS_MODE"); v != "" {
		c.Server.Mode = v
	}
}
