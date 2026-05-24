package gorm

import (
	"context"
	"fmt"
	"strings"

	"github.com/polaris-blog/blog/internal/model"
	"github.com/polaris-blog/blog/internal/repository"
	"gorm.io/gorm"
)

type PostRepo struct {
	db *gorm.DB
}

func NewPostRepo(db *gorm.DB) *PostRepo {
	return &PostRepo{db: db}
}

func (r *PostRepo) FindByID(ctx context.Context, id string) (*model.Post, error) {
	var post model.Post
	err := r.db.WithContext(ctx).Preload("Author").Preload("Category").Preload("Tags").First(&post, "id = ?", id).Error
	if err != nil {
		return nil, err
	}
	return &post, nil
}

func (r *PostRepo) FindBySlug(ctx context.Context, slug string) (*model.Post, error) {
	var post model.Post
	err := r.db.WithContext(ctx).Preload("Author").Preload("Category").Preload("Tags").First(&post, "slug = ?", slug).Error
	if err != nil {
		return nil, err
	}
	return &post, nil
}

func (r *PostRepo) List(ctx context.Context, opts repository.ListOptions) (*repository.ListResult[model.Post], error) {
	var posts []*model.Post
	var total int64

	query := r.db.WithContext(ctx).Model(&model.Post{})
	query = applyPostFilters(query, opts)

	if err := query.Count(&total).Error; err != nil {
		return nil, err
	}

	query = applyPagination(query, opts)
	query = applyOrder(query, opts)

	err := query.Preload("Author").Preload("Category").Preload("Tags").Find(&posts).Error
	if err != nil {
		return nil, err
	}

	return &repository.ListResult[model.Post]{Items: posts, Total: total}, nil
}

func (r *PostRepo) Create(ctx context.Context, post *model.Post) error {
	return r.db.WithContext(ctx).Create(post).Error
}

func (r *PostRepo) Update(ctx context.Context, post *model.Post) error {
	return r.db.WithContext(ctx).Save(post).Error
}

func (r *PostRepo) Delete(ctx context.Context, id string) error {
	return r.db.WithContext(ctx).Delete(&model.Post{}, "id = ?", id).Error
}

func (r *PostRepo) FindByCategory(ctx context.Context, categoryID string, opts repository.ListOptions) (*repository.ListResult[model.Post], error) {
	if opts.Filters == nil {
		opts.Filters = make(map[string]interface{})
	}
	opts.Filters["category_id"] = categoryID
	return r.List(ctx, opts)
}

func (r *PostRepo) FindByTag(ctx context.Context, tagID string, opts repository.ListOptions) (*repository.ListResult[model.Post], error) {
	var posts []*model.Post
	var total int64

	query := r.db.WithContext(ctx).Model(&model.Post{}).
		Joins("JOIN post_tags ON post_tags.post_id = posts.id").
		Where("post_tags.tag_id = ?", tagID)

	if opts.Status != "" {
		query = query.Where("posts.status = ?", opts.Status)
	}

	if err := query.Count(&total).Error; err != nil {
		return nil, err
	}

	query = applyPagination(query, opts)
	query = applyOrder(query, opts)

	err := query.Preload("Author").Preload("Category").Preload("Tags").Find(&posts).Error
	if err != nil {
		return nil, err
	}

	return &repository.ListResult[model.Post]{Items: posts, Total: total}, nil
}

func (r *PostRepo) FindByAuthor(ctx context.Context, authorID string, opts repository.ListOptions) (*repository.ListResult[model.Post], error) {
	if opts.Filters == nil {
		opts.Filters = make(map[string]interface{})
	}
	opts.Filters["author_id"] = authorID
	return r.List(ctx, opts)
}

func (r *PostRepo) Search(ctx context.Context, query string, opts repository.ListOptions) (*repository.ListResult[model.Post], error) {
	var posts []*model.Post
	var total int64

	q := r.db.WithContext(ctx).Model(&model.Post{}).
		Where("title LIKE ? OR content LIKE ?", "%"+query+"%", "%"+query+"%")

	if opts.Status != "" {
		q = q.Where("status = ?", opts.Status)
	}

	if err := q.Count(&total).Error; err != nil {
		return nil, err
	}

	q = applyPagination(q, opts)
	q = applyOrder(q, opts)

	err := q.Preload("Author").Preload("Category").Preload("Tags").Find(&posts).Error
	if err != nil {
		return nil, err
	}

	return &repository.ListResult[model.Post]{Items: posts, Total: total}, nil
}

func (r *PostRepo) Publish(ctx context.Context, id string) error {
	return r.db.WithContext(ctx).Model(&model.Post{}).Where("id = ?", id).
		Updates(map[string]interface{}{"status": "published"}).Error
}

func (r *PostRepo) Unpublish(ctx context.Context, id string) error {
	return r.db.WithContext(ctx).Model(&model.Post{}).Where("id = ?", id).
		Updates(map[string]interface{}{"status": "draft"}).Error
}

func (r *PostRepo) CountByStatus(ctx context.Context, status string) (int64, error) {
	var count int64
	err := r.db.WithContext(ctx).Model(&model.Post{}).Where("status = ?", status).Count(&count).Error
	return count, err
}

func applyPostFilters(query *gorm.DB, opts repository.ListOptions) *gorm.DB {
	if opts.Status != "" {
		query = query.Where("status = ?", opts.Status)
	}
	allowedColumns := map[string]bool{
		"type": true, "category_id": true, "author_id": true,
		"status": true, "slug": true, "featured": true,
	}
	for k, v := range opts.Filters {
		if !allowedColumns[k] {
			continue
		}
		query = query.Where(fmt.Sprintf("%s = ?", k), v)
	}
	return query
}

func applyPagination(query *gorm.DB, opts repository.ListOptions) *gorm.DB {
	if opts.Page <= 0 {
		opts.Page = 1
	}
	if opts.PageSize <= 0 {
		opts.PageSize = 20
	}
	return query.Offset((opts.Page - 1) * opts.PageSize).Limit(opts.PageSize)
}

func applyOrder(query *gorm.DB, opts repository.ListOptions) *gorm.DB {
	allowedOrderBy := map[string]bool{
		"created_at": true, "updated_at": true, "title": true, "id": true,
	}
	allowedOrder := map[string]bool{
		"ASC": true, "DESC": true, "asc": true, "desc": true,
	}
	orderBy := "created_at"
	if opts.OrderBy != "" && allowedOrderBy[opts.OrderBy] {
		orderBy = opts.OrderBy
	}
	order := "DESC"
	if opts.Order != "" && allowedOrder[opts.Order] {
		order = strings.ToUpper(opts.Order)
	}
	return query.Order(orderBy + " " + order)
}
