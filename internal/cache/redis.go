package cache

import (
	"context"
	"encoding/json"
	"time"

	"github.com/redis/go-redis/v9"
)

type RedisCache struct {
	client *redis.Client
	prefix string
}

func NewRedisCache(addr, password string, db int, prefix string) (*RedisCache, error) {
	client := redis.NewClient(&redis.Options{
		Addr:     addr,
		Password: password,
		DB:       db,
	})

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	if err := client.Ping(ctx).Err(); err != nil {
		return nil, err
	}

	return &RedisCache{
		client: client,
		prefix: prefix,
	}, nil
}

func (c *RedisCache) key(k string) string {
	if c.prefix == "" {
		return k
	}
	return c.prefix + ":" + k
}

func (c *RedisCache) Get(key string) (interface{}, bool) {
	ctx := context.Background()
	data, err := c.client.Get(ctx, c.key(key)).Bytes()
	if err != nil {
		return nil, false
	}

	var item struct {
		Value interface{} `json:"v"`
	}
	if err := json.Unmarshal(data, &item); err != nil {
		return nil, false
	}
	return item.Value, true
}

func (c *RedisCache) Set(key string, value interface{}, ttl time.Duration) {
	ctx := context.Background()
	item := struct {
		Value interface{} `json:"v"`
	}{Value: value}
	data, err := json.Marshal(item)
	if err != nil {
		return
	}
	c.client.Set(ctx, c.key(key), data, ttl)
}

func (c *RedisCache) Delete(key string) {
	ctx := context.Background()
	c.client.Del(ctx, c.key(key))
}

func (c *RedisCache) Clear() {
	ctx := context.Background()
	if c.prefix == "" {
		c.client.FlushDB(ctx)
		return
	}
	iter := c.client.Scan(ctx, 0, c.prefix+":*", 0).Iterator()
	for iter.Next(ctx) {
		c.client.Del(ctx, iter.Val())
	}
}

func (c *RedisCache) Close() {
	c.client.Close()
}
