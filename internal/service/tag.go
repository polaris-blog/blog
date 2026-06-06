package service

import (
	"context"
	"encoding/json"
	"fmt"
	"time"

	"github.com/google/uuid"
	"github.com/polaris-blog/blog/internal/cache"
	"github.com/polaris-blog/blog/internal/model"
	"github.com/polaris-blog/blog/internal/plugin"
	"github.com/polaris-blog/blog/internal/repository"
)

type TagService struct {
	tags     repository.TagRepository
	eventBus *plugin.EventBus
	cache    cache.Cache
	cacheTTL time.Duration
}

func NewTagService(tags repository.TagRepository, eventBus *plugin.EventBus, c cache.Cache, cacheTTL time.Duration) *TagService {
	return &TagService{tags: tags, eventBus: eventBus, cache: c, cacheTTL: cacheTTL}
}

type CreateTagInput struct {
	Name string `json:"name"`
	Slug string `json:"slug"`
}

func (s *TagService) Create(ctx context.Context, input CreateTagInput) (*model.Tag, error) {
	if input.Name == "" {
		return nil, fmt.Errorf("name is required")
	}
	if input.Slug == "" {
		input.Slug = input.Name
	}

	existing, _ := s.tags.FindBySlug(ctx, input.Slug)
	if existing != nil {
		return nil, fmt.Errorf("tag with slug %q already exists", input.Slug)
	}

	tag := &model.Tag{
		ID:        uuid.New().String(),
		Name:      input.Name,
		Slug:      input.Slug,
		CreatedAt: time.Now(),
		UpdatedAt: time.Now(),
	}

	if err := s.tags.Create(ctx, tag); err != nil {
		return nil, fmt.Errorf("create tag: %w", err)
	}

	s.cache.Delete("tags:list")
	return tag, nil
}

func (s *TagService) List(ctx context.Context) ([]*model.Tag, error) {
	key := "tags:list"
	if val, ok := s.cache.Get(key); ok {
		if tags, ok := val.([]*model.Tag); ok {
			return tags, nil
		}
		var tags []*model.Tag
		if data, err := json.Marshal(val); err == nil {
			if json.Unmarshal(data, &tags) == nil {
				return tags, nil
			}
		}
	}
	tags, err := s.tags.List(ctx)
	if err != nil {
		return nil, err
	}
	s.cache.Set(key, tags, s.cacheTTL)
	return tags, nil
}

func (s *TagService) GetBySlug(ctx context.Context, slug string) (*model.Tag, error) {
	return s.tags.FindBySlug(ctx, slug)
}

func (s *TagService) Delete(ctx context.Context, id string) error {
	_, err := s.tags.FindByID(ctx, id)
	if err != nil {
		return fmt.Errorf("tag not found")
	}
	if err := s.tags.Delete(ctx, id); err != nil {
		return err
	}
	s.cache.Delete("tags:list")
	return nil
}

func (s *TagService) Count(ctx context.Context) (int64, error) {
	return s.tags.Count(ctx)
}
