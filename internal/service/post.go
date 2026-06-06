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

type PostService struct {
	posts    repository.PostRepository
	tags     repository.TagRepository
	eventBus *plugin.EventBus
	cache    cache.Cache
	cacheTTL time.Duration
}

func NewPostService(posts repository.PostRepository, tags repository.TagRepository, eventBus *plugin.EventBus, c cache.Cache, cacheTTL time.Duration) *PostService {
	return &PostService{posts: posts, tags: tags, eventBus: eventBus, cache: c, cacheTTL: cacheTTL}
}

func cacheGet[T any](c cache.Cache, key string) (*T, bool) {
	val, ok := c.Get(key)
	if !ok {
		return nil, false
	}
	if v, ok := val.(*T); ok {
		return v, true
	}
	if v, ok := val.(T); ok {
		return &v, true
	}
	data, err := json.Marshal(val)
	if err != nil {
		return nil, false
	}
	var result T
	if json.Unmarshal(data, &result) != nil {
		return nil, false
	}
	return &result, true
}

func (s *PostService) GetByID(ctx context.Context, id string) (*model.Post, error) {
	key := "post:id:" + id
	if val, ok := cacheGet[model.Post](s.cache, key); ok {
		return val, nil
	}
	post, err := s.posts.FindByID(ctx, id)
	if err != nil {
		return nil, err
	}
	s.cache.Set(key, post, s.cacheTTL)
	return post, nil
}

func (s *PostService) GetBySlug(ctx context.Context, slug string) (*model.Post, error) {
	key := "post:slug:" + slug
	if val, ok := cacheGet[model.Post](s.cache, key); ok {
		return val, nil
	}
	post, err := s.posts.FindBySlug(ctx, slug)
	if err != nil {
		return nil, err
	}
	s.cache.Set(key, post, s.cacheTTL)
	return post, nil
}

func (s *PostService) List(ctx context.Context, page, pageSize int, status string) ([]*model.Post, int64, error) {
	opts := repository.ListOptions{
		Page:     page,
		PageSize: pageSize,
		OrderBy:  "created_at",
		Order:    "DESC",
		Status:   status,
	}
	result, err := s.posts.List(ctx, opts)
	if err != nil {
		return nil, 0, err
	}
	return result.Items, result.Total, nil
}

func (s *PostService) ListByCategory(ctx context.Context, categoryID string, page, pageSize int) ([]*model.Post, int64, error) {
	opts := repository.ListOptions{
		Page:     page,
		PageSize: pageSize,
		OrderBy:  "created_at",
		Order:    "DESC",
		Status:   "published",
	}
	result, err := s.posts.FindByCategory(ctx, categoryID, opts)
	if err != nil {
		return nil, 0, err
	}
	return result.Items, result.Total, nil
}

func (s *PostService) ListByTag(ctx context.Context, tagID string, page, pageSize int) ([]*model.Post, int64, error) {
	opts := repository.ListOptions{
		Page:     page,
		PageSize: pageSize,
		OrderBy:  "created_at",
		Order:    "DESC",
		Status:   "published",
	}
	result, err := s.posts.FindByTag(ctx, tagID, opts)
	if err != nil {
		return nil, 0, err
	}
	return result.Items, result.Total, nil
}

func (s *PostService) ListByType(ctx context.Context, postType string, page, pageSize int, status string) ([]*model.Post, int64, error) {
	opts := repository.ListOptions{
		Page:     page,
		PageSize: pageSize,
		OrderBy:  "created_at",
		Order:    "DESC",
		Status:   status,
		Filters:  map[string]interface{}{"type": postType},
	}
	result, err := s.posts.List(ctx, opts)
	if err != nil {
		return nil, 0, err
	}
	return result.Items, result.Total, nil
}

func (s *PostService) Search(ctx context.Context, query string, page, pageSize int) ([]*model.Post, int64, error) {
	opts := repository.ListOptions{
		Page:     page,
		PageSize: pageSize,
		OrderBy:  "created_at",
		Order:    "DESC",
		Status:   "published",
	}
	result, err := s.posts.Search(ctx, query, opts)
	if err != nil {
		return nil, 0, err
	}
	return result.Items, result.Total, nil
}

type CreatePostInput struct {
	Title        string   `json:"title"`
	Slug         string   `json:"slug"`
	Content      string   `json:"content"`
	Excerpt      string   `json:"excerpt"`
	Status       string   `json:"status"`
	Type         string   `json:"type"`
	Format       string   `json:"format"`
	CoverImage   string   `json:"cover_image"`
	AuthorID     string   `json:"author_id"`
	CategoryID   string   `json:"category_id"`
	IsPinned     bool     `json:"is_pinned"`
	AllowComment bool     `json:"allow_comment"`
	TagIDs       []string `json:"tag_ids"`
}

func (s *PostService) Create(ctx context.Context, input CreatePostInput) (*model.Post, error) {
	post := &model.Post{
		ID:           uuid.New().String(),
		Title:        input.Title,
		Slug:         input.Slug,
		Content:      input.Content,
		Excerpt:      input.Excerpt,
		Status:       input.Status,
		Type:         input.Type,
		Format:       input.Format,
		CoverImage:   input.CoverImage,
		AuthorID:     input.AuthorID,
		CategoryID:   input.CategoryID,
		IsPinned:     input.IsPinned,
		AllowComment: input.AllowComment,
		CreatedAt:    time.Now(),
		UpdatedAt:    time.Now(),
	}

	if post.Status == "" {
		post.Status = "draft"
	}
	if post.Type == "" {
		post.Type = "post"
	}
	if post.Format == "" {
		post.Format = "markdown"
	}

	if len(input.TagIDs) > 0 {
		tagList, err := s.tags.FindByIDs(ctx, input.TagIDs)
		if err == nil {
			post.Tags = make([]model.Tag, len(tagList))
			for i, t := range tagList {
				post.Tags[i] = *t
			}
		}
	}

	_ = s.eventBus.EmitHook(ctx, plugin.HookPostBeforeCreate, post)

	if err := s.posts.Create(ctx, post); err != nil {
		return nil, fmt.Errorf("create post: %w", err)
	}

	_ = s.eventBus.EmitHook(ctx, plugin.HookPostAfterCreate, post)
	s.invalidatePostCache(post)

	return post, nil
}

type UpdatePostInput struct {
	Title        *string  `json:"title"`
	Slug         *string  `json:"slug"`
	Content      *string  `json:"content"`
	Excerpt      *string  `json:"excerpt"`
	Status       *string  `json:"status"`
	Type         *string  `json:"type"`
	CoverImage   *string  `json:"cover_image"`
	CategoryID   *string  `json:"category_id"`
	IsPinned     *bool    `json:"is_pinned"`
	AllowComment *bool    `json:"allow_comment"`
	TagIDs       []string `json:"tag_ids"`
}

func (s *PostService) Update(ctx context.Context, id string, input UpdatePostInput) (*model.Post, error) {
	post, err := s.posts.FindByID(ctx, id)
	if err != nil {
		return nil, fmt.Errorf("find post: %w", err)
	}

	if input.Title != nil {
		post.Title = *input.Title
	}
	if input.Slug != nil {
		post.Slug = *input.Slug
	}
	if input.Content != nil {
		post.Content = *input.Content
	}
	if input.Excerpt != nil {
		post.Excerpt = *input.Excerpt
	}
	if input.Status != nil {
		post.Status = *input.Status
	}
	if input.Type != nil {
		post.Type = *input.Type
	}
	if input.CoverImage != nil {
		post.CoverImage = *input.CoverImage
	}
	if input.CategoryID != nil {
		post.CategoryID = *input.CategoryID
	}
	if input.IsPinned != nil {
		post.IsPinned = *input.IsPinned
	}
	if input.AllowComment != nil {
		post.AllowComment = *input.AllowComment
	}

	if input.TagIDs != nil {
		tagList, err := s.tags.FindByIDs(ctx, input.TagIDs)
		if err == nil {
			post.Tags = make([]model.Tag, len(tagList))
			for i, t := range tagList {
				post.Tags[i] = *t
			}
		}
	}

	post.UpdatedAt = time.Now()

	_ = s.eventBus.EmitHook(ctx, plugin.HookPostBeforeUpdate, post)

	if err := s.posts.Update(ctx, post); err != nil {
		return nil, fmt.Errorf("update post: %w", err)
	}

	_ = s.eventBus.EmitHook(ctx, plugin.HookPostAfterUpdate, post)
	s.invalidatePostCache(post)

	return post, nil
}

func (s *PostService) Delete(ctx context.Context, id string) error {
	post, err := s.posts.FindByID(ctx, id)
	if err != nil {
		return fmt.Errorf("find post: %w", err)
	}

	_ = s.eventBus.EmitHook(ctx, plugin.HookPostBeforeDelete, id)

	if err := s.posts.Delete(ctx, id); err != nil {
		return fmt.Errorf("delete post: %w", err)
	}

	_ = s.eventBus.EmitHook(ctx, plugin.HookPostAfterDelete, id)
	s.invalidatePostCache(post)

	return nil
}

func (s *PostService) Publish(ctx context.Context, id string) error {
	_ = s.eventBus.EmitHook(ctx, plugin.HookPostBeforePublish, id)

	now := time.Now()
	post, err := s.posts.FindByID(ctx, id)
	if err != nil {
		return err
	}
	post.Status = "published"
	post.PublishedAt = &now
	post.UpdatedAt = now

	if err := s.posts.Update(ctx, post); err != nil {
		return err
	}

	_ = s.eventBus.EmitHook(ctx, plugin.HookPostAfterPublish, post)
	s.invalidatePostCache(post)

	return nil
}

func (s *PostService) Unpublish(ctx context.Context, id string) error {
	post, err := s.posts.FindByID(ctx, id)
	if err != nil {
		return err
	}
	err = s.posts.Unpublish(ctx, id)
	if err == nil {
		s.invalidatePostCache(post)
	}
	return err
}

func (s *PostService) CountByStatus(ctx context.Context, status string) (int64, error) {
	return s.posts.CountByStatus(ctx, status)
}

func (s *PostService) CountByType(ctx context.Context, postType string, status string) (int64, error) {
	return s.posts.CountByType(ctx, postType, status)
}

func (s *PostService) ListSlugs(ctx context.Context, postType string, status string, limit int) ([]*model.Post, error) {
	return s.posts.ListSlugs(ctx, postType, status, limit)
}

func (s *PostService) invalidatePostCache(post *model.Post) {
	s.cache.Delete("post:id:" + post.ID)
	if post.Slug != "" {
		s.cache.Delete("post:slug:" + post.Slug)
	}
}
