package services

import (
	"github.com/polaris/blog/internal/models"
	"go.uber.org/zap"
	"gorm.io/gorm"
)

type NavigationService struct {
	db     *gorm.DB
	logger *zap.Logger
}

func NewNavigationService(db *gorm.DB, logger *zap.Logger) *NavigationService {
	return &NavigationService{db: db, logger: logger}
}

// SeedBuiltins creates default built-in navigations if they don't exist.
func (s *NavigationService) SeedBuiltins() {
	builtins := []models.Navigation{
		{Name: "首页", URL: "/", SortOrder: 0, IsBuiltin: true, IsActive: true},
		{Name: "分类", URL: "/categories", SortOrder: 1, IsBuiltin: true, IsActive: true},
		{Name: "标签", URL: "/tags", SortOrder: 2, IsBuiltin: true, IsActive: true},
		{Name: "归档", URL: "/archives", SortOrder: 3, IsBuiltin: true, IsActive: true},
	}
	for _, nav := range builtins {
		var count int64
		s.db.Model(&models.Navigation{}).Where("url = ? AND is_builtin = ?", nav.URL, true).Count(&count)
		if count == 0 {
			if err := s.db.Create(&nav).Error; err != nil {
				s.logger.Error("Failed to seed built-in navigation", zap.String("name", nav.Name), zap.Error(err))
			}
		}
	}
}

func (s *NavigationService) List() ([]models.Navigation, error) {
	var navs []models.Navigation
	if err := s.db.Where("parent_id IS NULL").Order("sort_order ASC, id ASC").Find(&navs).Error; err != nil {
		return nil, err
	}
	
	for i := range navs {
		var children []models.Navigation
		s.db.Where("parent_id = ?", navs[i].ID).Order("sort_order ASC, id ASC").Find(&children)
		navs[i].Children = children
	}
	
	return navs, nil
}

func (s *NavigationService) GetAll() ([]models.Navigation, error) {
	var navs []models.Navigation
	if err := s.db.Order("sort_order ASC, id ASC").Find(&navs).Error; err != nil {
		return nil, err
	}
	return navs, nil
}

func (s *NavigationService) GetActive() ([]models.Navigation, error) {
	var navs []models.Navigation
	if err := s.db.Where("is_active = ? AND parent_id IS NULL", true).Order("sort_order ASC, id ASC").Find(&navs).Error; err != nil {
		return nil, err
	}
	
	for i := range navs {
		var children []models.Navigation
		s.db.Where("is_active = ? AND parent_id = ?", true, navs[i].ID).Order("sort_order ASC, id ASC").Find(&children)
		navs[i].Children = children
	}
	
	return navs, nil
}

func (s *NavigationService) GetByID(id uint) (*models.Navigation, error) {
	var nav models.Navigation
	if err := s.db.First(&nav, id).Error; err != nil {
		return nil, err
	}
	return &nav, nil
}

func (s *NavigationService) Create(nav *models.Navigation) error {
	return s.db.Create(nav).Error
}

func (s *NavigationService) Update(nav *models.Navigation) error {
	return s.db.Save(nav).Error
}

func (s *NavigationService) Delete(id uint) error {
	s.db.Where("parent_id = ?", id).Delete(&models.Navigation{})
	return s.db.Delete(&models.Navigation{}, id).Error
}

func (s *NavigationService) UpdateOrder(items []struct {
	ID        uint  `json:"id"`
	SortOrder int   `json:"sort_order"`
	ParentID  *uint `json:"parent_id"`
}) error {
	return s.db.Transaction(func(tx *gorm.DB) error {
		for _, item := range items {
			if err := tx.Model(&models.Navigation{}).Where("id = ?", item.ID).Updates(map[string]interface{}{
				"sort_order": item.SortOrder,
				"parent_id":  item.ParentID,
			}).Error; err != nil {
				return err
			}
		}
		return nil
	})
}
