package gorm

import (
	"context"
	"fmt"

	"github.com/polaris-blog/blog/internal/model"
	"github.com/polaris-blog/blog/internal/repository"
	"gorm.io/gorm"
	"gorm.io/gorm/clause"
)

type CategoryRepo struct {
	db *gorm.DB
}

func NewCategoryRepo(db *gorm.DB) *CategoryRepo {
	return &CategoryRepo{db: db}
}

func (r *CategoryRepo) FindByID(ctx context.Context, id string) (*model.Category, error) {
	var cat model.Category
	err := r.db.WithContext(ctx).First(&cat, "id = ?", id).Error
	if err != nil {
		return nil, err
	}
	return &cat, nil
}

func (r *CategoryRepo) FindBySlug(ctx context.Context, slug string) (*model.Category, error) {
	var cat model.Category
	err := r.db.WithContext(ctx).First(&cat, "slug = ?", slug).Error
	if err != nil {
		return nil, err
	}
	return &cat, nil
}

func (r *CategoryRepo) List(ctx context.Context) ([]*model.Category, error) {
	var cats []*model.Category
	err := r.db.WithContext(ctx).Order("sort_order ASC, name ASC").Find(&cats).Error
	return cats, err
}

func (r *CategoryRepo) Create(ctx context.Context, category *model.Category) error {
	return r.db.WithContext(ctx).Create(category).Error
}

func (r *CategoryRepo) Update(ctx context.Context, category *model.Category) error {
	return r.db.WithContext(ctx).Save(category).Error
}

func (r *CategoryRepo) Delete(ctx context.Context, id string) error {
	return r.db.WithContext(ctx).Delete(&model.Category{}, "id = ?", id).Error
}

func (r *CategoryRepo) Count(ctx context.Context) (int64, error) {
	var count int64
	err := r.db.WithContext(ctx).Model(&model.Category{}).Count(&count).Error
	return count, err
}

type TagRepo struct {
	db *gorm.DB
}

func NewTagRepo(db *gorm.DB) *TagRepo {
	return &TagRepo{db: db}
}

func (r *TagRepo) FindByID(ctx context.Context, id string) (*model.Tag, error) {
	var tag model.Tag
	err := r.db.WithContext(ctx).First(&tag, "id = ?", id).Error
	if err != nil {
		return nil, err
	}
	return &tag, nil
}

func (r *TagRepo) FindBySlug(ctx context.Context, slug string) (*model.Tag, error) {
	var tag model.Tag
	err := r.db.WithContext(ctx).First(&tag, "slug = ?", slug).Error
	if err != nil {
		return nil, err
	}
	return &tag, nil
}

func (r *TagRepo) FindByIDs(ctx context.Context, ids []string) ([]*model.Tag, error) {
	var tags []*model.Tag
	if len(ids) == 0 {
		return tags, nil
	}
	err := r.db.WithContext(ctx).Where("id IN ?", ids).Find(&tags).Error
	return tags, err
}

func (r *TagRepo) List(ctx context.Context) ([]*model.Tag, error) {
	var tags []*model.Tag
	err := r.db.WithContext(ctx).Order("name ASC").Find(&tags).Error
	return tags, err
}

func (r *TagRepo) Create(ctx context.Context, tag *model.Tag) error {
	return r.db.WithContext(ctx).Create(tag).Error
}

func (r *TagRepo) Update(ctx context.Context, tag *model.Tag) error {
	return r.db.WithContext(ctx).Save(tag).Error
}

func (r *TagRepo) Delete(ctx context.Context, id string) error {
	return r.db.WithContext(ctx).Delete(&model.Tag{}, "id = ?", id).Error
}

func (r *TagRepo) Count(ctx context.Context) (int64, error) {
	var count int64
	err := r.db.WithContext(ctx).Model(&model.Tag{}).Count(&count).Error
	return count, err
}

type CommentRepo struct {
	db *gorm.DB
}

func NewCommentRepo(db *gorm.DB) *CommentRepo {
	return &CommentRepo{db: db}
}

func (r *CommentRepo) FindByID(ctx context.Context, id string) (*model.Comment, error) {
	var c model.Comment
	err := r.db.WithContext(ctx).First(&c, "id = ?", id).Error
	if err != nil {
		return nil, err
	}
	return &c, nil
}

func (r *CommentRepo) List(ctx context.Context, opts repository.ListOptions) (*repository.ListResult[model.Comment], error) {
	var comments []*model.Comment
	var total int64

	query := r.db.WithContext(ctx).Model(&model.Comment{})
	if opts.Status != "" {
		query = query.Where("status = ?", opts.Status)
	}
	allowedColumns := map[string]bool{"post_id": true, "status": true, "author_id": true}
	for k, v := range opts.Filters {
		if !allowedColumns[k] {
			continue
		}
		query = query.Where(fmt.Sprintf("\"%s\" = ?", k), v)
	}
	if err := query.Count(&total).Error; err != nil {
		return nil, err
	}

	query = applyPagination(query, opts)
	query = applyOrder(query, opts)

	err := query.Preload("Author").Find(&comments).Error
	if err != nil {
		return nil, err
	}

	return &repository.ListResult[model.Comment]{Items: comments, Total: total}, nil
}

func (r *CommentRepo) FindByPost(ctx context.Context, postID string, opts repository.ListOptions) (*repository.ListResult[model.Comment], error) {
	if opts.Filters == nil {
		opts.Filters = make(map[string]interface{})
	}
	opts.Filters["post_id"] = postID
	if opts.Status == "" {
		opts.Status = model.CommentStatusApproved
	}
	return r.List(ctx, opts)
}

func (r *CommentRepo) Create(ctx context.Context, comment *model.Comment) error {
	return r.db.WithContext(ctx).Create(comment).Error
}

func (r *CommentRepo) Update(ctx context.Context, comment *model.Comment) error {
	return r.db.WithContext(ctx).Save(comment).Error
}

func (r *CommentRepo) Delete(ctx context.Context, id string) error {
	return r.db.WithContext(ctx).Delete(&model.Comment{}, "id = ?", id).Error
}

func (r *CommentRepo) UpdateStatus(ctx context.Context, id string, status string) error {
	return r.db.WithContext(ctx).Model(&model.Comment{}).Where("id = ?", id).
		Update("status", status).Error
}

func (r *CommentRepo) CountByStatus(ctx context.Context, status string) (int64, error) {
	var count int64
	err := r.db.WithContext(ctx).Model(&model.Comment{}).Where("status = ?", status).Count(&count).Error
	return count, err
}

type MediaRepo struct {
	db *gorm.DB
}

func NewMediaRepo(db *gorm.DB) *MediaRepo {
	return &MediaRepo{db: db}
}

func (r *MediaRepo) FindByID(ctx context.Context, id string) (*model.Media, error) {
	var m model.Media
	err := r.db.WithContext(ctx).First(&m, "id = ?", id).Error
	if err != nil {
		return nil, err
	}
	return &m, nil
}

func (r *MediaRepo) List(ctx context.Context, opts repository.ListOptions) (*repository.ListResult[model.Media], error) {
	var items []*model.Media
	var total int64

	query := r.db.WithContext(ctx).Model(&model.Media{})
	if err := query.Count(&total).Error; err != nil {
		return nil, err
	}

	query = applyPagination(query, opts)
	query = applyOrder(query, opts)

	err := query.Find(&items).Error
	if err != nil {
		return nil, err
	}

	return &repository.ListResult[model.Media]{Items: items, Total: total}, nil
}

func (r *MediaRepo) Create(ctx context.Context, media *model.Media) error {
	return r.db.WithContext(ctx).Create(media).Error
}

func (r *MediaRepo) Delete(ctx context.Context, id string) error {
	return r.db.WithContext(ctx).Delete(&model.Media{}, "id = ?", id).Error
}

type OptionRepo struct {
	db *gorm.DB
}

func NewOptionRepo(db *gorm.DB) *OptionRepo {
	return &OptionRepo{db: db}
}

func (r *OptionRepo) Get(ctx context.Context, key string) (string, error) {
	var opt model.Option
	err := r.db.WithContext(ctx).First(&opt, "key = ?", key).Error
	if err != nil {
		return "", err
	}
	return opt.Value, nil
}

func (r *OptionRepo) GetMulti(ctx context.Context, keys []string) (map[string]string, error) {
	if len(keys) == 0 {
		return map[string]string{}, nil
	}
	var opts []model.Option
	err := r.db.WithContext(ctx).Where("key IN ?", keys).Find(&opts).Error
	if err != nil {
		return nil, err
	}
	result := make(map[string]string, len(opts))
	for _, o := range opts {
		result[o.Key] = o.Value
	}
	return result, nil
}

func (r *OptionRepo) Set(ctx context.Context, key string, value string) error {
	opt := model.Option{
		Key:   key,
		Value: value,
	}
	return r.db.WithContext(ctx).Clauses(clause.OnConflict{
		Columns:   []clause.Column{{Name: "key"}},
		DoUpdates: clause.AssignmentColumns([]string{"value"}),
	}).Create(&opt).Error
}

func (r *OptionRepo) Delete(ctx context.Context, key string) error {
	return r.db.WithContext(ctx).Delete(&model.Option{}, "key = ?", key).Error
}
