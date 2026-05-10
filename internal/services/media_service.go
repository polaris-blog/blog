package services

import (
	"errors"
	"fmt"
	"io"
	"mime/multipart"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/google/uuid"
	"github.com/polaris/blog/internal/config"
	"github.com/polaris/blog/internal/models"
	"go.uber.org/zap"
	"gorm.io/gorm"
)

type MediaService struct {
	db     *gorm.DB
	cfg    *config.Config
	logger *zap.Logger
}

var allowedImageTypes = map[string]bool{
	"image/jpeg": true,
	"image/png":  true,
	"image/gif":  true,
	"image/webp": true,
}

var allowedFileTypes = map[string]bool{
	"application/pdf":    true,
	"application/zip":    true,
	"application/x-zip-compressed": true,
}

func NewMediaService(db *gorm.DB, cfg *config.Config, logger *zap.Logger) *MediaService {
	return &MediaService{db: db, cfg: cfg, logger: logger}
}

type MediaListParams struct {
	Page     int
	PageSize int
	Type     string
	Keyword  string
}

func (s *MediaService) List(params MediaListParams) ([]models.Media, int64, error) {
	var media []models.Media
	var total int64

	query := s.db.Model(&models.Media{})

	if params.Type == "image" {
		query = query.Where("type = ?", "image")
	} else if params.Type == "file" {
		query = query.Where("type != ?", "image")
	}

	if params.Keyword != "" {
		keyword := "%" + params.Keyword + "%"
		query = query.Where("name LIKE ?", keyword)
	}

	query.Count(&total)

	offset := (params.Page - 1) * params.PageSize
	if err := query.Order("created_at DESC").Offset(offset).Limit(params.PageSize).Find(&media).Error; err != nil {
		return nil, 0, err
	}

	return media, total, nil
}

func (s *MediaService) Upload(file *multipart.FileHeader) (*models.Media, error) {
	if file.Size > 10*1024*1024 {
		return nil, errors.New("文件大小不能超过 10MB")
	}

	src, err := file.Open()
	if err != nil {
		return nil, err
	}
	defer src.Close()

	buffer := make([]byte, 512)
	if _, err := src.Read(buffer); err != nil {
		return nil, err
	}
	src.Seek(0, 0)

	mimeType := file.Header.Get("Content-Type")
	isImage := allowedImageTypes[mimeType]
	isAllowed := isImage || allowedFileTypes[mimeType]

	if !isAllowed {
		return nil, errors.New("不支持的文件格式")
	}

	ext := filepath.Ext(file.Filename)
	filename := fmt.Sprintf("%d_%s%s", time.Now().Unix(), uuid.New().String()[:8], ext)

	uploadPath := s.cfg.Storage.Path
	if err := os.MkdirAll(uploadPath, 0755); err != nil {
		return nil, err
	}

	filePath := filepath.Join(uploadPath, filename)
	dst, err := os.Create(filePath)
	if err != nil {
		return nil, err
	}
	defer dst.Close()

	if _, err := io.Copy(dst, src); err != nil {
		return nil, err
	}

	mediaType := "file"
	if isImage {
		mediaType = "image"
	}

	media := &models.Media{
		Name:     file.Filename,
		Path:     filePath,
		URL:      "/uploads/" + filename,
		Type:     mediaType,
		Size:     file.Size,
		MimeType: mimeType,
	}

	if err := s.db.Create(media).Error; err != nil {
		os.Remove(filePath)
		return nil, err
	}

	return media, nil
}

func (s *MediaService) Delete(id uint) error {
	var media models.Media
	if err := s.db.First(&media, id).Error; err != nil {
		return err
	}

	if err := os.Remove(media.Path); err != nil && !os.IsNotExist(err) {
		s.logger.Warn("Failed to delete file", zap.String("path", media.Path), zap.Error(err))
	}

	if media.Thumbnail != "" {
		os.Remove(media.Thumbnail)
	}

	return s.db.Delete(&media).Error
}

func (s *MediaService) GetByID(id uint) (*models.Media, error) {
	var media models.Media
	if err := s.db.First(&media, id).Error; err != nil {
		return nil, err
	}
	return &media, nil
}

func (s *MediaService) Rename(id uint, name string) error {
	return s.db.Model(&models.Media{}).Where("id = ?", id).Update("name", name).Error
}

func (s *MediaService) GetByIDs(ids []uint) ([]models.Media, error) {
	var media []models.Media
	if err := s.db.Where("id IN ?", ids).Find(&media).Error; err != nil {
		return nil, err
	}
	return media, nil
}

func (s *MediaService) GetStorageUsage() (int64, error) {
	var totalSize int64
	s.db.Model(&models.Media{}).Select("COALESCE(SUM(size), 0)").Scan(&totalSize)
	return totalSize, nil
}

func (s *MediaService) IsImage(mimeType string) bool {
	return allowedImageTypes[mimeType]
}

func (s *MediaService) GetFileExtension(filename string) string {
	return strings.ToLower(filepath.Ext(filename))
}
