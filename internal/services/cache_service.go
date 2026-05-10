package services

import (
	"encoding/json"
	"time"

	"github.com/polaris/blog/internal/config"
	"go.uber.org/zap"
)

type CacheService struct {
	cfg    *config.Config
	logger *zap.Logger
	store  map[string]cacheItem
}

type cacheItem struct {
	Data      []byte
	ExpiresAt time.Time
}

func NewCacheService(cfg *config.Config, logger *zap.Logger) *CacheService {
	return &CacheService{
		cfg:    cfg,
		logger: logger,
		store:  make(map[string]cacheItem),
	}
}

func (c *CacheService) Get(key string) (interface{}, bool) {
	if !c.cfg.Cache.Enabled {
		return nil, false
	}

	item, exists := c.store[key]
	if !exists {
		return nil, false
	}

	if time.Now().After(item.ExpiresAt) {
		delete(c.store, key)
		return nil, false
	}

	var value interface{}
	if err := json.Unmarshal(item.Data, &value); err != nil {
		return nil, false
	}

	return value, true
}

func (c *CacheService) Set(key string, value interface{}) error {
	if !c.cfg.Cache.Enabled {
		return nil
	}

	data, err := json.Marshal(value)
	if err != nil {
		return err
	}

	c.store[key] = cacheItem{
		Data:      data,
		ExpiresAt: time.Now().Add(c.cfg.Cache.Expiry),
	}

	return nil
}

func (c *CacheService) Delete(key string) {
	delete(c.store, key)
}

func (c *CacheService) DeleteByPrefix(prefix string) {
	for key := range c.store {
		if len(key) >= len(prefix) && key[:len(prefix)] == prefix {
			delete(c.store, key)
		}
	}
}

func (c *CacheService) Clear() {
	c.store = make(map[string]cacheItem)
}

func (c *CacheService) GetOrSet(key string, fn func() (interface{}, error)) (interface{}, error) {
	if value, exists := c.Get(key); exists {
		return value, nil
	}

	value, err := fn()
	if err != nil {
		return nil, err
	}

	if err := c.Set(key, value); err != nil {
		c.logger.Warn("Failed to cache value", zap.String("key", key), zap.Error(err))
	}

	return value, nil
}

func (c *CacheService) GetString(key string) (string, bool) {
	value, exists := c.Get(key)
	if !exists {
		return "", false
	}

	if str, ok := value.(string); ok {
		return str, true
	}

	return "", false
}

func (c *CacheService) SetString(key, value string) error {
	return c.Set(key, value)
}

func (c *CacheService) GetBytes(key string) ([]byte, bool) {
	value, exists := c.Get(key)
	if !exists {
		return nil, false
	}

	if bytes, ok := value.([]byte); ok {
		return bytes, true
	}

	return nil, false
}

func (c *CacheService) SetBytes(key string, value []byte) error {
	return c.Set(key, value)
}
