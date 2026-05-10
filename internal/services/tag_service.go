package services

import (
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/polaris/blog/internal/models"
	"go.uber.org/zap"
	"gorm.io/gorm"
)

type TagService struct {
	db     *gorm.DB
	logger *zap.Logger
}

func NewTagService(db *gorm.DB, logger *zap.Logger) *TagService {
	return &TagService{db: db, logger: logger}
}

func (s *TagService) List() ([]models.Tag, error) {
	var tags []models.Tag
	if err := s.db.Order("name ASC").Find(&tags).Error; err != nil {
		return nil, err
	}
	return tags, nil
}

func (s *TagService) GetByID(id uint) (*models.Tag, error) {
	var tag models.Tag
	if err := s.db.First(&tag, id).Error; err != nil {
		return nil, err
	}
	return &tag, nil
}

func (s *TagService) GetBySlug(slug string) (*models.Tag, error) {
	var tag models.Tag
	if err := s.db.Where("slug = ?", slug).First(&tag).Error; err != nil {
		return nil, err
	}
	return &tag, nil
}

func (s *TagService) Create(tag *models.Tag) error {
	return s.db.Create(tag).Error
}

func (s *TagService) Update(tag *models.Tag) error {
	return s.db.Save(tag).Error
}

func (s *TagService) Delete(id uint) error {
	s.db.Exec("DELETE FROM post_tags WHERE tag_id = ?", id)
	return s.db.Delete(&models.Tag{}, id).Error
}

func (s *TagService) Merge(sourceID, targetID uint) error {
	if sourceID == targetID {
		return errors.New("cannot merge tag with itself")
	}

	var source, target models.Tag
	if err := s.db.First(&source, sourceID).Error; err != nil {
		return err
	}
	if err := s.db.First(&target, targetID).Error; err != nil {
		return err
	}

	s.db.Exec(`
		INSERT INTO post_tags (post_id, tag_id)
		SELECT post_id, ? FROM post_tags WHERE tag_id = ?
		ON CONFLICT DO NOTHING
	`, targetID, sourceID)

	s.db.Exec("DELETE FROM post_tags WHERE tag_id = ?", sourceID)

	s.UpdatePostCount(sourceID)
	s.UpdatePostCount(targetID)

	return s.db.Delete(&source).Error
}

func (s *TagService) UpdatePostCount(id uint) error {
	var count int64
	s.db.Table("post_tags").Where("tag_id = ?", id).Count(&count)
	return s.db.Model(&models.Tag{}).Where("id = ?", id).Update("post_count", count).Error
}

func (s *TagService) AdminList() ([]models.Tag, error) {
	return s.List()
}

func (s *TagService) GetOrCreate(name string) (*models.Tag, error) {
	var tag models.Tag
	err := s.db.Where("name = ?", name).First(&tag).Error
	if err == gorm.ErrRecordNotFound {
		slug := strings.ToLower(strings.ReplaceAll(name, " ", "-"))
		
		var count int64
		s.db.Model(&models.Tag{}).Where("slug = ?", slug).Count(&count)
		if count > 0 {
			slug = slug + "-" + fmt.Sprintf("%d", time.Now().Unix())
		}
		
		tag = models.Tag{Name: name, Slug: slug}
		if err := s.db.Create(&tag).Error; err != nil {
			return nil, err
		}
		return &tag, nil
	}
	if err != nil {
		return nil, err
	}
	return &tag, nil
}
