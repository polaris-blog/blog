package services

import (
	"strings"

	"github.com/polaris/blog/internal/models"
	"go.uber.org/zap"
	"gorm.io/gorm"
)

type SearchService struct {
	db     *gorm.DB
	logger *zap.Logger
}

type SearchResult struct {
	Posts []models.Post `json:"posts"`
	Total int64         `json:"total"`
}

func NewSearchService(db *gorm.DB, logger *zap.Logger) *SearchService {
	return &SearchService{db: db, logger: logger}
}

type SearchParams struct {
	Keyword  string
	Page     int
	PageSize int
}

func (s *SearchService) Search(params SearchParams) (*SearchResult, error) {
	if len(strings.TrimSpace(params.Keyword)) < 2 {
		return &SearchResult{Posts: []models.Post{}, Total: 0}, nil
	}

	var posts []models.Post
	var total int64

	keyword := "%" + strings.ToLower(params.Keyword) + "%"

	query := s.db.Model(&models.Post{}).
		Preload("Author").
		Preload("Category").
		Preload("Tags").
		Where("status = ?", models.PostStatusPublished).
		Where(
			s.db.Where("LOWER(title) LIKE ?", keyword).
				Or("LOWER(content) LIKE ?", keyword).
				Or("id IN (SELECT post_id FROM post_tags JOIN tags ON post_tags.tag_id = tags.id WHERE LOWER(tags.name) LIKE ?)", keyword).
				Or("category_id IN (SELECT id FROM categories WHERE LOWER(name) LIKE ?)", keyword),
		)

	query.Count(&total)

	offset := (params.Page - 1) * params.PageSize
	if err := query.Order("published_at DESC").Offset(offset).Limit(params.PageSize).Find(&posts).Error; err != nil {
		return nil, err
	}

	return &SearchResult{Posts: posts, Total: total}, nil
}

func (s *SearchService) SearchByTitle(keyword string, page, pageSize int) ([]models.Post, int64, error) {
	var posts []models.Post
	var total int64

	query := s.db.Model(&models.Post{}).
		Where("status = ?", models.PostStatusPublished).
		Where("LOWER(title) LIKE ?", "%"+strings.ToLower(keyword)+"%")

	query.Count(&total)

	offset := (page - 1) * pageSize
	if err := query.Order("published_at DESC").Offset(offset).Limit(pageSize).Find(&posts).Error; err != nil {
		return nil, 0, err
	}

	return posts, total, nil
}

func (s *SearchService) SearchByContent(keyword string, page, pageSize int) ([]models.Post, int64, error) {
	var posts []models.Post
	var total int64

	query := s.db.Model(&models.Post{}).
		Where("status = ?", models.PostStatusPublished).
		Where("LOWER(content) LIKE ?", "%"+strings.ToLower(keyword)+"%")

	query.Count(&total)

	offset := (page - 1) * pageSize
	if err := query.Order("published_at DESC").Offset(offset).Limit(pageSize).Find(&posts).Error; err != nil {
		return nil, 0, err
	}

	return posts, total, nil
}

func (s *SearchService) HighlightKeyword(content, keyword string) string {
	if keyword == "" {
		return content
	}

	replacer := strings.NewReplacer(
		strings.ToLower(keyword), "<mark>"+keyword+"</mark>",
		strings.ToUpper(keyword), "<mark>"+keyword+"</mark>",
		strings.Title(keyword), "<mark>"+strings.Title(keyword)+"</mark>",
	)
	return replacer.Replace(content)
}

func (s *SearchService) GetExcerpt(content, keyword string, length int) string {
	if len(content) <= length {
		return content
	}

	lowerContent := strings.ToLower(content)
	lowerKeyword := strings.ToLower(keyword)

	idx := strings.Index(lowerContent, lowerKeyword)
	if idx == -1 {
		return content[:length] + "..."
	}

	start := idx - length/3
	if start < 0 {
		start = 0
	}

	end := start + length
	if end > len(content) {
		end = len(content)
	}

	excerpt := content[start:end]
	if start > 0 {
		excerpt = "..." + excerpt
	}
	if end < len(content) {
		excerpt = excerpt + "..."
	}

	return excerpt
}
