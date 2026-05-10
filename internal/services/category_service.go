package services

import (
	"github.com/polaris/blog/internal/models"
	"go.uber.org/zap"
	"gorm.io/gorm"
)

type CategoryService struct {
	db     *gorm.DB
	logger *zap.Logger
}

func NewCategoryService(db *gorm.DB, logger *zap.Logger) *CategoryService {
	return &CategoryService{db: db, logger: logger}
}

func (s *CategoryService) List() ([]models.Category, error) {
	var categories []models.Category
	if err := s.db.Order("sort_order DESC, id ASC").Find(&categories).Error; err != nil {
		return nil, err
	}
	return categories, nil
}

func (s *CategoryService) GetByID(id uint) (*models.Category, error) {
	var category models.Category
	if err := s.db.First(&category, id).Error; err != nil {
		return nil, err
	}
	return &category, nil
}

func (s *CategoryService) GetBySlug(slug string) (*models.Category, error) {
	var category models.Category
	if err := s.db.Where("slug = ?", slug).First(&category).Error; err != nil {
		return nil, err
	}
	return &category, nil
}

func (s *CategoryService) Create(category *models.Category) error {
	return s.db.Create(category).Error
}

func (s *CategoryService) Update(category *models.Category) error {
	return s.db.Save(category).Error
}

func (s *CategoryService) Delete(id uint) error {
	s.db.Model(&models.Post{}).Where("category_id = ?", id).Update("category_id", nil)
	return s.db.Delete(&models.Category{}, id).Error
}

func (s *CategoryService) UpdatePostCount(id uint) error {
	var count int64
	s.db.Model(&models.Post{}).Where("category_id = ?", id).Count(&count)
	return s.db.Model(&models.Category{}).Where("id = ?", id).Update("post_count", count).Error
}

func (s *CategoryService) AdminList() ([]models.Category, error) {
	return s.List()
}
