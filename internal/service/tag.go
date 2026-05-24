package service

import (
	"context"
	"fmt"
	"time"

	"github.com/google/uuid"
	"github.com/polaris-blog/blog/internal/model"
	"github.com/polaris-blog/blog/internal/plugin"
	"github.com/polaris-blog/blog/internal/repository"
)

type TagService struct {
	tags     repository.TagRepository
	eventBus *plugin.EventBus
}

func NewTagService(tags repository.TagRepository, eventBus *plugin.EventBus) *TagService {
	return &TagService{tags: tags, eventBus: eventBus}
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

	return tag, nil
}

func (s *TagService) List(ctx context.Context) ([]*model.Tag, error) {
	return s.tags.List(ctx)
}

func (s *TagService) GetBySlug(ctx context.Context, slug string) (*model.Tag, error) {
	return s.tags.FindBySlug(ctx, slug)
}

func (s *TagService) Delete(ctx context.Context, id string) error {
	_, err := s.tags.FindByID(ctx, id)
	if err != nil {
		return fmt.Errorf("tag not found")
	}
	return s.tags.Delete(ctx, id)
}

func (s *TagService) Count(ctx context.Context) (int64, error) {
	return s.tags.Count(ctx)
}
