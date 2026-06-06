package repository

import (
	"context"

	"github.com/polaris-blog/blog/internal/model"
)

type ListOptions struct {
	Page     int
	PageSize int
	OrderBy  string
	Order    string
	Status   string
	Filters  map[string]interface{}
}

type ListResult[T any] struct {
	Items []*T
	Total int64
}

type PostRepository interface {
	FindByID(ctx context.Context, id string) (*model.Post, error)
	FindBySlug(ctx context.Context, slug string) (*model.Post, error)
	List(ctx context.Context, opts ListOptions) (*ListResult[model.Post], error)
	Create(ctx context.Context, post *model.Post) error
	Update(ctx context.Context, post *model.Post) error
	Delete(ctx context.Context, id string) error
	FindByCategory(ctx context.Context, categoryID string, opts ListOptions) (*ListResult[model.Post], error)
	FindByTag(ctx context.Context, tagID string, opts ListOptions) (*ListResult[model.Post], error)
	Search(ctx context.Context, query string, opts ListOptions) (*ListResult[model.Post], error)
	Publish(ctx context.Context, id string) error
	Unpublish(ctx context.Context, id string) error
	CountByStatus(ctx context.Context, status string) (int64, error)
	CountByType(ctx context.Context, postType string, status string) (int64, error)
	ListSlugs(ctx context.Context, postType string, status string, limit int) ([]*model.Post, error)
}

type UserRepository interface {
	FindByID(ctx context.Context, id string) (*model.User, error)
	FindByUsername(ctx context.Context, username string) (*model.User, error)
	FindByEmail(ctx context.Context, email string) (*model.User, error)
	List(ctx context.Context, opts ListOptions) (*ListResult[model.User], error)
	Create(ctx context.Context, user *model.User) error
	Update(ctx context.Context, user *model.User) error
	Delete(ctx context.Context, id string) error
	UpdateLastLogin(ctx context.Context, id string) error
}

type CategoryRepository interface {
	FindByID(ctx context.Context, id string) (*model.Category, error)
	FindBySlug(ctx context.Context, slug string) (*model.Category, error)
	List(ctx context.Context) ([]*model.Category, error)
	Create(ctx context.Context, category *model.Category) error
	Update(ctx context.Context, category *model.Category) error
	Delete(ctx context.Context, id string) error
	Count(ctx context.Context) (int64, error)
}

type TagRepository interface {
	FindByID(ctx context.Context, id string) (*model.Tag, error)
	FindBySlug(ctx context.Context, slug string) (*model.Tag, error)
	FindByIDs(ctx context.Context, ids []string) ([]*model.Tag, error)
	List(ctx context.Context) ([]*model.Tag, error)
	Create(ctx context.Context, tag *model.Tag) error
	Update(ctx context.Context, tag *model.Tag) error
	Delete(ctx context.Context, id string) error
	Count(ctx context.Context) (int64, error)
}

type CommentRepository interface {
	FindByID(ctx context.Context, id string) (*model.Comment, error)
	List(ctx context.Context, opts ListOptions) (*ListResult[model.Comment], error)
	FindByPost(ctx context.Context, postID string, opts ListOptions) (*ListResult[model.Comment], error)
	Create(ctx context.Context, comment *model.Comment) error
	Update(ctx context.Context, comment *model.Comment) error
	Delete(ctx context.Context, id string) error
	UpdateStatus(ctx context.Context, id string, status string) error
	CountByStatus(ctx context.Context, status string) (int64, error)
}

type MediaRepository interface {
	FindByID(ctx context.Context, id string) (*model.Media, error)
	List(ctx context.Context, opts ListOptions) (*ListResult[model.Media], error)
	Create(ctx context.Context, media *model.Media) error
	Delete(ctx context.Context, id string) error
}

type OptionRepository interface {
	Get(ctx context.Context, key string) (string, error)
	GetMulti(ctx context.Context, keys []string) (map[string]string, error)
	Set(ctx context.Context, key string, value string) error
	Delete(ctx context.Context, key string) error
}
