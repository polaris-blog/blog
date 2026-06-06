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
		Addr:         addr,
		Password:     password,
		DB:           db,
		DialTimeout:  5 * time.Second,
		ReadTimeout:  3 * time.Second,
		WriteTimeout: 3 * time.Second,
		PoolSize:     10,
		MinIdleConns: 3,
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
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	data, err := c.client.Get(ctx, c.key(key)).Bytes()
	if err != nil {
		return nil, false
	}

	var item struct {
		Value json.RawMessage `json:"v"`
	}
	if err := json.Unmarshal(data, &item); err != nil {
		return nil, false
	}

	var result interface{}
	if err := json.Unmarshal(item.Value, &result); err != nil {
		return nil, false
	}
	return result, true
}

func (c *RedisCache) Set(key string, value interface{}, ttl time.Duration) {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	valBytes, err := json.Marshal(value)
	if err != nil {
		return
	}
	item := struct {
		Value json.RawMessage `json:"v"`
	}{Value: json.RawMessage(valBytes)}
	data, err := json.Marshal(item)
	if err != nil {
		return
	}
	c.client.Set(ctx, c.key(key), data, ttl)
}

func (c *RedisCache) Delete(key string) {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	c.client.Del(ctx, c.key(key))
}

func (c *RedisCache) Clear() {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	if c.prefix == "" {
		c.client.FlushDB(ctx)
		return
	}

	iter := c.client.Scan(ctx, 0, c.prefix+":*", 100).Iterator()
	for iter.Next(ctx) {
		c.client.Del(ctx, iter.Val())
	}
}

func (c *RedisCache) Close() {
	c.client.Close()
}
