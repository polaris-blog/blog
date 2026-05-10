package services

import (
	"regexp"
	"strings"
	"time"

	"github.com/polaris/blog/internal/models"
	"go.uber.org/zap"
	"gorm.io/gorm"
)

func generateSlug(title string) string {
	re := regexp.MustCompile(`[^\w\s-]`)
	slug := re.ReplaceAllString(strings.ToLower(title), "")
	slug = regexp.MustCompile(`\s+`).ReplaceAllString(slug, "-")
	slug = regexp.MustCompile(`-+`).ReplaceAllString(slug, "-")
	return strings.Trim(slug, "-")
}

type PageService struct {
	db     *gorm.DB
	logger *zap.Logger
}

func NewPageService(db *gorm.DB, logger *zap.Logger) *PageService {
	return &PageService{db: db, logger: logger}
}

type PageListParams struct {
	Page     int
	PageSize int
	Status   string
	Keyword  string
}

func (s *PageService) List(params PageListParams) ([]models.Page, int64, error) {
	var pages []models.Page
	var total int64

	query := s.db.Model(&models.Page{})

	if params.Status != "" {
		query = query.Where("status = ?", params.Status)
	}

	if params.Keyword != "" {
		query = query.Where("title LIKE ?", "%"+params.Keyword+"%")
	}

	query.Count(&total)

	offset := (params.Page - 1) * params.PageSize
	if err := query.Order("id DESC").Offset(offset).Limit(params.PageSize).Find(&pages).Error; err != nil {
		return nil, 0, err
	}

	return pages, total, nil
}

func (s *PageService) GetByID(id uint) (*models.Page, error) {
	var page models.Page
	if err := s.db.First(&page, id).Error; err != nil {
		return nil, err
	}
	return &page, nil
}

func (s *PageService) GetBySlug(slug string) (*models.Page, error) {
	var page models.Page
	if err := s.db.Where("slug = ? AND status = ?", slug, models.PageStatusPublished).First(&page).Error; err != nil {
		return nil, err
	}
	return &page, nil
}

func (s *PageService) Create(page *models.Page) error {
	if page.Slug == "" {
		page.Slug = generateSlug(page.Title)
	}
	return s.db.Create(page).Error
}

func (s *PageService) Update(page *models.Page) error {
	return s.db.Save(page).Error
}

func (s *PageService) Delete(id uint) error {
	return s.db.Delete(&models.Page{}, id).Error
}

func (s *PageService) Publish(id uint) error {
	now := time.Now()
	return s.db.Model(&models.Page{}).Where("id = ?", id).Updates(map[string]interface{}{
		"status":       models.PageStatusPublished,
		"published_at": &now,
	}).Error
}

func (s *PageService) GetNavPages() ([]models.Page, error) {
	var pages []models.Page
	if err := s.db.Where("is_in_nav = ? AND status = ?", true, models.PageStatusPublished).
		Order("nav_sort ASC, id ASC").Find(&pages).Error; err != nil {
		return nil, err
	}
	return pages, nil
}

func (s *PageService) IncrementViewCount(id uint) error {
	return s.db.Model(&models.Page{}).Where("id = ?", id).
		UpdateColumn("view_count", gorm.Expr("view_count + 1")).Error
}
