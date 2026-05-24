package cache

import (
	"fmt"
	"time"

	"github.com/polaris-blog/blog/internal/config"
)

func New(cfg config.CacheConfig) (Cache, error) {
	switch cfg.Driver {
	case "redis":
		if cfg.Redis.Addr == "" {
			return nil, fmt.Errorf("redis cache requires redis.addr")
		}
		prefix := "polaris"
		return NewRedisCache(cfg.Redis.Addr, cfg.Redis.Password, cfg.Redis.DB, prefix)
	case "memory", "":
		return NewMemoryCache(), nil
	default:
		return nil, fmt.Errorf("unknown cache driver: %s", cfg.Driver)
	}
}

func TTL(cfg config.CacheConfig) time.Duration {
	if cfg.TTL <= 0 {
		return time.Hour
	}
	return time.Duration(cfg.TTL) * time.Second
}
