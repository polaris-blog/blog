package services

import (
	"strings"
	"time"

	"github.com/polaris/blog/internal/models"
	"go.uber.org/zap"
	"gorm.io/gorm"
)

type PostService struct {
	db     *gorm.DB
	logger *zap.Logger
}

func NewPostService(db *gorm.DB, logger *zap.Logger) *PostService {
	return &PostService{db: db, logger: logger}
}

type PostListParams struct {
	Page     int
	PageSize int
	Status   string
	Category uint
	Tag      uint
	Keyword  string
	OrderBy  string
}

func (s *PostService) List(params PostListParams) ([]models.Post, int64, error) {
	var posts []models.Post
	var total int64

	query := s.db.Model(&models.Post{}).Preload("Author").Preload("Category").Preload("Tags")

	if params.Status != "" {
		query = query.Where("status = ?", params.Status)
	} else {
		query = query.Where("status = ?", models.PostStatusPublished)
	}

	if params.Category > 0 {
		query = query.Where("category_id = ?", params.Category)
	}

	if params.Tag > 0 {
		query = query.Joins("JOIN post_tags ON post_tags.post_id = posts.id AND post_tags.tag_id = ?", params.Tag)
	}

	if params.Keyword != "" {
		keyword := "%" + params.Keyword + "%"
		query = query.Where("title LIKE ? OR content LIKE ?", keyword, keyword)
	}

	query.Count(&total)

	order := "is_top DESC, published_at DESC"
	if params.OrderBy != "" {
		order = params.OrderBy
	}

	offset := (params.Page - 1) * params.PageSize
	if err := query.Order(order).Offset(offset).Limit(params.PageSize).Find(&posts).Error; err != nil {
		return nil, 0, err
	}

	return posts, total, nil
}

func (s *PostService) GetByID(id uint) (*models.Post, error) {
	var post models.Post
	if err := s.db.Preload("Author").Preload("Category").Preload("Tags").First(&post, id).Error; err != nil {
		return nil, err
	}
	return &post, nil
}

func (s *PostService) GetBySlug(slug string) (*models.Post, error) {
	var post models.Post
	if err := s.db.Preload("Author").Preload("Category").Preload("Tags").Where("slug = ?", slug).First(&post).Error; err != nil {
		return nil, err
	}

	s.db.Model(&post).UpdateColumn("view_count", gorm.Expr("view_count + 1"))

	return &post, nil
}

func (s *PostService) Create(post *models.Post, tagIDs []uint) error {
	if post.Slug == "" {
		post.Slug = s.generateSlug(post.Title)
	}

	if post.Status == models.PostStatusPublished && post.PublishedAt == nil {
		now := time.Now()
		post.PublishedAt = &now
	}

	if err := s.db.Create(post).Error; err != nil {
		return err
	}

	if len(tagIDs) > 0 {
		var tags []models.Tag
		if err := s.db.Find(&tags, tagIDs).Error; err != nil {
			return err
		}
		s.db.Model(post).Association("Tags").Replace(tags)
		for _, tagID := range tagIDs {
			s.updateTagPostCount(tagID)
		}
	}

	if post.CategoryID != nil {
		s.updateCategoryPostCount(*post.CategoryID)
	}

	s.saveVersion(post)

	return nil
}

func (s *PostService) Update(post *models.Post, tagIDs []uint) error {
	existing, err := s.GetByID(post.ID)
	if err != nil {
		return err
	}

	if post.Slug == "" {
		post.Slug = existing.Slug
	}

	oldCategoryID := existing.CategoryID

	s.saveVersion(existing)

	if err := s.db.Save(post).Error; err != nil {
		return err
	}

	if tagIDs != nil {
		var oldTags []models.Tag
		s.db.Model(existing).Association("Tags").Find(&oldTags)
		
		var oldTagIDs []uint
		for _, tag := range oldTags {
			oldTagIDs = append(oldTagIDs, tag.ID)
		}
		
		var tags []models.Tag
		if len(tagIDs) > 0 {
			if err := s.db.Find(&tags, tagIDs).Error; err != nil {
				return err
			}
		}
		s.db.Model(post).Association("Tags").Replace(tags)
		
		allTagIDs := append(oldTagIDs, tagIDs...)
		for _, tagID := range allTagIDs {
			s.updateTagPostCount(tagID)
		}
	}

	if oldCategoryID != post.CategoryID {
		if oldCategoryID != nil {
			s.updateCategoryPostCount(*oldCategoryID)
		}
		if post.CategoryID != nil {
			s.updateCategoryPostCount(*post.CategoryID)
		}
	}

	return nil
}

func (s *PostService) updateTagPostCount(tagID uint) {
	var count int64
	s.db.Table("post_tags").Where("tag_id = ?", tagID).Count(&count)
	s.db.Model(&models.Tag{}).Where("id = ?", tagID).Update("post_count", count)
}

func (s *PostService) updateCategoryPostCount(categoryID uint) {
	var count int64
	s.db.Model(&models.Post{}).Where("category_id = ? AND status = ?", categoryID, models.PostStatusPublished).Count(&count)
	s.db.Model(&models.Category{}).Where("id = ?", categoryID).Update("post_count", count)
}

func (s *PostService) Delete(id uint) error {
	return s.db.Delete(&models.Post{}, id).Error
}

func (s *PostService) Publish(id uint) error {
	now := time.Now()
	return s.db.Model(&models.Post{}).Where("id = ?", id).Updates(map[string]interface{}{
		"status":      models.PostStatusPublished,
		"published_at": now,
	}).Error
}

func (s *PostService) Schedule(id uint, publishAt time.Time) error {
	return s.db.Model(&models.Post{}).Where("id = ?", id).Updates(map[string]interface{}{
		"status":      models.PostStatusScheduled,
		"published_at": publishAt,
	}).Error
}

func (s *PostService) GetVersions(postID uint) ([]models.PostVersion, error) {
	var versions []models.PostVersion
	if err := s.db.Where("post_id = ?", postID).Order("version DESC").Limit(50).Find(&versions).Error; err != nil {
		return nil, err
	}
	return versions, nil
}

func (s *PostService) saveVersion(post *models.Post) error {
	var maxVersion int
	s.db.Model(&models.PostVersion{}).Where("post_id = ?", post.ID).Select("COALESCE(MAX(version), 0)").Scan(&maxVersion)

	version := models.PostVersion{
		PostID:  post.ID,
		Title:   post.Title,
		Content: post.Content,
		Version: maxVersion + 1,
	}

	return s.db.Create(&version).Error
}

func (s *PostService) generateSlug(title string) string {
	slug := strings.ToLower(title)
	slug = strings.ReplaceAll(slug, " ", "-")
	slug = strings.ReplaceAll(slug, "_", "-")

	var count int64
	s.db.Model(&models.Post{}).Where("slug LIKE ?", slug+"%").Count(&count)
	if count > 0 {
		slug = slug + "-" + string(rune(count+1))
	}

	return slug
}

func (s *PostService) IncrementViewCount(id uint) error {
	return s.db.Model(&models.Post{}).Where("id = ?", id).UpdateColumn("view_count", gorm.Expr("view_count + 1")).Error
}

func (s *PostService) CheckPassword(post *models.Post, password string) bool {
	return post.Password == password
}

func (s *PostService) GetRecentPosts(limit int) ([]models.Post, error) {
	var posts []models.Post
	err := s.db.Where("status = ?", models.PostStatusPublished).
		Order("published_at DESC").
		Limit(limit).
		Find(&posts).Error
	return posts, err
}

func (s *PostService) GetArchives() (map[string]int64, error) {
	type Archive struct {
		Year  int
		Month int
		Count int64
	}

	var archives []Archive
	err := s.db.Model(&models.Post{}).
		Select("EXTRACT(YEAR FROM published_at) as year, EXTRACT(MONTH FROM published_at) as month, COUNT(*) as count").
		Where("status = ?", models.PostStatusPublished).
		Group("year, month").
		Order("year DESC, month DESC").
		Find(&archives).Error

	result := make(map[string]int64)
	for _, a := range archives {
		key := time.Date(a.Year, time.Month(a.Month), 1, 0, 0, 0, 0, time.UTC).Format("2006-01")
		result[key] = a.Count
	}

	return result, err
}

func (s *PostService) AdminList(params PostListParams) ([]models.Post, int64, error) {
	var posts []models.Post
	var total int64

	query := s.db.Model(&models.Post{}).Preload("Author").Preload("Category").Preload("Tags")

	if params.Status != "" {
		query = query.Where("status = ?", params.Status)
	}

	if params.Category > 0 {
		query = query.Where("category_id = ?", params.Category)
	}

	if params.Keyword != "" {
		keyword := "%" + params.Keyword + "%"
		query = query.Where("title LIKE ? OR content LIKE ?", keyword, keyword)
	}

	query.Count(&total)

	order := "created_at DESC"
	if params.OrderBy != "" {
		order = params.OrderBy
	}

	offset := (params.Page - 1) * params.PageSize
	if err := query.Order(order).Offset(offset).Limit(params.PageSize).Find(&posts).Error; err != nil {
		return nil, 0, err
	}

	return posts, total, nil
}

func (s *PostService) PublishScheduledPosts() error {
	return s.db.Model(&models.Post{}).
		Where("status = ? AND published_at <= ?", models.PostStatusScheduled, time.Now()).
		Update("status", models.PostStatusPublished).Error
}

func (s *PostService) VerifyPassword(postID uint, password string) (bool, error) {
	var post models.Post
	if err := s.db.Select("password").First(&post, postID).Error; err != nil {
		return false, err
	}
	return post.Password == password, nil
}

func (s *PostService) GetByIDAdmin(id uint) (*models.Post, error) {
	var post models.Post
	if err := s.db.Preload("Author").Preload("Category").Preload("Tags").First(&post, id).Error; err != nil {
		return nil, err
	}
	return &post, nil
}

func (s *PostService) GetBySlugAdmin(slug string) (*models.Post, error) {
	var post models.Post
	if err := s.db.Preload("Author").Preload("Category").Preload("Tags").Where("slug = ?", slug).First(&post).Error; err != nil {
		return nil, err
	}
	return &post, nil
}
