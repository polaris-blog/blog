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

type CategoryService struct {
	categories repository.CategoryRepository
	eventBus   *plugin.EventBus
}

func NewCategoryService(categories repository.CategoryRepository, eventBus *plugin.EventBus) *CategoryService {
	return &CategoryService{categories: categories, eventBus: eventBus}
}

type CreateCategoryInput struct {
	Name        string `json:"name"`
	Slug        string `json:"slug"`
	Description string `json:"description"`
	ParentID    string `json:"parent_id"`
	SortOrder   int    `json:"sort_order"`
}

func (s *CategoryService) Create(ctx context.Context, input CreateCategoryInput) (*model.Category, error) {
	if input.Name == "" {
		return nil, fmt.Errorf("name is required")
	}
	if input.Slug == "" {
		input.Slug = input.Name
	}

	existing, _ := s.categories.FindBySlug(ctx, input.Slug)
	if existing != nil {
		return nil, fmt.Errorf("category with slug %q already exists", input.Slug)
	}

	category := &model.Category{
		ID:          uuid.New().String(),
		Name:        input.Name,
		Slug:        input.Slug,
		Description: input.Description,
		ParentID:    input.ParentID,
		SortOrder:   input.SortOrder,
		CreatedAt:   time.Now(),
		UpdatedAt:   time.Now(),
	}

	if err := s.categories.Create(ctx, category); err != nil {
		return nil, fmt.Errorf("create category: %w", err)
	}

	return category, nil
}

func (s *CategoryService) List(ctx context.Context) ([]*model.Category, error) {
	return s.categories.List(ctx)
}

func (s *CategoryService) GetBySlug(ctx context.Context, slug string) (*model.Category, error) {
	return s.categories.FindBySlug(ctx, slug)
}

func (s *CategoryService) Delete(ctx context.Context, id string) error {
	_, err := s.categories.FindByID(ctx, id)
	if err != nil {
		return fmt.Errorf("category not found")
	}
	return s.categories.Delete(ctx, id)
}

func (s *CategoryService) Count(ctx context.Context) (int64, error) {
	cats, err := s.categories.List(ctx)
	if err != nil {
		return 0, err
	}
	return int64(len(cats)), nil
}
