package services

import (
	"github.com/polaris/blog/internal/models"
	"go.uber.org/zap"
	"gorm.io/gorm"
)

type SettingService struct {
	db     *gorm.DB
	logger *zap.Logger
}

func NewSettingService(db *gorm.DB, logger *zap.Logger) *SettingService {
	return &SettingService{db: db, logger: logger}
}

func (s *SettingService) Get(key string) (string, error) {
	var setting models.Setting
	if err := s.db.Where("key = ?", key).First(&setting).Error; err != nil {
		return "", err
	}
	return setting.Value, nil
}

func (s *SettingService) Set(key, value string) error {
	var setting models.Setting
	if err := s.db.Where("key = ?", key).First(&setting).Error; err != nil {
		if err == gorm.ErrRecordNotFound {
			setting = models.Setting{Key: key, Value: value}
			return s.db.Create(&setting).Error
		}
		return err
	}
	return s.db.Model(&setting).Update("value", value).Error
}

func (s *SettingService) GetAll() (map[string]string, error) {
	var settings []models.Setting
	if err := s.db.Find(&settings).Error; err != nil {
		return nil, err
	}

	result := make(map[string]string)
	for _, s := range settings {
		result[s.Key] = s.Value
	}

	return result, nil
}

var allowedSettingKeys = map[string]bool{
	"site_title": true, "site_subtitle": true, "site_description": true, "site_keywords": true,
	"site_url": true, "site_icp": true, "site_logo": true, "site_favicon": true,
	"site_footer": true, "site_analytics": true,
	"author_name": true, "author_email": true, "author_avatar": true, "author_description": true,
	"about_content": true,
}

func (s *SettingService) Update(settings map[string]string) error {
	for key, value := range settings {
		if !allowedSettingKeys[key] {
			s.logger.Warn("Blocked setting update with unknown key", zap.String("key", key))
			continue
		}
		if err := s.Set(key, value); err != nil {
			return err
		}
	}
	return nil
}

func (s *SettingService) GetPublic() (map[string]string, error) {
	settings, err := s.GetAll()
	if err != nil {
		return nil, err
	}

	// Return a curated set of settings with original key names
	// so templates can access them directly (e.g. {{.settings.site_title}})
	publicKeys := []string{
		"site_title", "site_subtitle", "site_description", "site_keywords",
		"site_logo", "site_favicon", "site_footer",
		"site_url", "site_icp",
		"author_name", "author_email", "author_avatar", "author_description",
		"about_content",
		"turnstile_enabled", "turnstile_site_key",
	}

	public := make(map[string]string, len(publicKeys))
	for _, key := range publicKeys {
		if v, ok := settings[key]; ok {
			public[key] = v
		}
	}

	return public, nil
}

func (s *SettingService) ListFriendLinks() ([]models.FriendLink, error) {
	var links []models.FriendLink
	if err := s.db.Where("is_active = ?", true).Order("sort_order DESC, id ASC").Find(&links).Error; err != nil {
		return nil, err
	}
	return links, nil
}

func (s *SettingService) CreateFriendLink(link *models.FriendLink) error {
	return s.db.Create(link).Error
}

func (s *SettingService) UpdateFriendLink(link *models.FriendLink) error {
	return s.db.Save(link).Error
}

func (s *SettingService) DeleteFriendLink(id uint) error {
	return s.db.Delete(&models.FriendLink{}, id).Error
}

func (s *SettingService) GetAllFriendLinks() ([]models.FriendLink, error) {
	var links []models.FriendLink
	if err := s.db.Order("sort_order DESC, id ASC").Find(&links).Error; err != nil {
		return nil, err
	}
	return links, nil
}
