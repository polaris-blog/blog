package services

import (
	"errors"
	"regexp"
	"strings"

	"github.com/polaris/blog/internal/models"
	"go.uber.org/zap"
	"gorm.io/gorm"
)

type CommentService struct {
	db     *gorm.DB
	logger *zap.Logger
	sensitiveWords []string
}

func NewCommentService(db *gorm.DB, logger *zap.Logger) *CommentService {
	return &CommentService{
		db:     db,
		logger: logger,
		sensitiveWords: []string{"spam", "广告", "垃圾"},
	}
}

type CommentListParams struct {
	Page     int
	PageSize int
	PostID   uint
	Status   string
	OrderBy  string
}

func (s *CommentService) List(params CommentListParams) ([]models.Comment, int64, error) {
	var comments []models.Comment
	var total int64

	query := s.db.Model(&models.Comment{}).Where("status = ?", models.CommentStatusApproved)

	if params.PostID > 0 {
		query = query.Where("post_id = ?", params.PostID)
	}

	query.Count(&total)

	order := "created_at DESC"
	if params.OrderBy != "" {
		order = params.OrderBy
	}

	offset := (params.Page - 1) * params.PageSize
	if err := query.Order(order).Offset(offset).Limit(params.PageSize).Preload("Replies").Find(&comments).Error; err != nil {
		return nil, 0, err
	}

	return comments, total, nil
}

func (s *CommentService) GetByID(id uint) (*models.Comment, error) {
	var comment models.Comment
	if err := s.db.First(&comment, id).Error; err != nil {
		return nil, err
	}
	return &comment, nil
}

func (s *CommentService) Create(comment *models.Comment, moderationMode string) error {
	comment.Content = s.filterSensitiveWords(comment.Content)

	if moderationMode == "pre" {
		comment.Status = models.CommentStatusPending
	} else {
		comment.Status = models.CommentStatusApproved
	}

	return s.db.Create(comment).Error
}

func (s *CommentService) Approve(id uint) error {
	return s.db.Model(&models.Comment{}).Where("id = ?", id).Update("status", models.CommentStatusApproved).Error
}

func (s *CommentService) MarkSpam(id uint) error {
	return s.db.Model(&models.Comment{}).Where("id = ?", id).Update("status", models.CommentStatusSpam).Error
}

func (s *CommentService) Delete(id uint) error {
	s.db.Where("parent_id = ?", id).Delete(&models.Comment{})
	return s.db.Delete(&models.Comment{}, id).Error
}

func (s *CommentService) Like(id uint, ip string) error {
	var existing models.CommentLike
	if err := s.db.Where("comment_id = ? AND ip = ?", id, ip).First(&existing).Error; err == nil {
		return errors.New("already liked")
	}

	like := models.CommentLike{CommentID: id, IP: ip}
	if err := s.db.Create(&like).Error; err != nil {
		return err
	}

	return s.db.Model(&models.Comment{}).Where("id = ?", id).UpdateColumn("like_count", gorm.Expr("like_count + 1")).Error
}

func (s *CommentService) AdminList(params CommentListParams) ([]models.Comment, int64, error) {
	var comments []models.Comment
	var total int64

	query := s.db.Model(&models.Comment{}).Preload("Post")

	if params.Status != "" {
		query = query.Where("status = ?", params.Status)
	}

	if params.PostID > 0 {
		query = query.Where("post_id = ?", params.PostID)
	}

	query.Count(&total)

	order := "created_at DESC"
	if params.OrderBy != "" {
		order = params.OrderBy
	}

	offset := (params.Page - 1) * params.PageSize
	if err := query.Order(order).Offset(offset).Limit(params.PageSize).Find(&comments).Error; err != nil {
		return nil, 0, err
	}

	return comments, total, nil
}

func (s *CommentService) BatchAction(action string, ids []uint) error {
	switch action {
	case "approve":
		return s.db.Model(&models.Comment{}).Where("id IN ?", ids).Update("status", models.CommentStatusApproved).Error
	case "spam":
		return s.db.Model(&models.Comment{}).Where("id IN ?", ids).Update("status", models.CommentStatusSpam).Error
	case "delete":
		return s.db.Where("id IN ?", ids).Delete(&models.Comment{}).Error
	default:
		return errors.New("invalid action")
	}
}

func (s *CommentService) filterSensitiveWords(content string) string {
	for _, word := range s.sensitiveWords {
		re := regexp.MustCompile(`(?i)`+regexp.QuoteMeta(word))
		content = re.ReplaceAllString(content, "***")
	}
	return content
}

func (s *CommentService) GetPendingCount() int64 {
	var count int64
	s.db.Model(&models.Comment{}).Where("status = ?", models.CommentStatusPending).Count(&count)
	return count
}

func (s *CommentService) GetNestedComments(postID uint) ([]models.Comment, error) {
	var comments []models.Comment
	if err := s.db.Where("post_id = ? AND status = ? AND parent_id IS NULL", postID, models.CommentStatusApproved).
		Order("created_at DESC").
		Preload("Replies", "status = ?", models.CommentStatusApproved).
		Find(&comments).Error; err != nil {
		return nil, err
	}
	return comments, nil
}

func (s *CommentService) ValidateEmail(email string) bool {
	emailRegex := regexp.MustCompile(`^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$`)
	return emailRegex.MatchString(email)
}

func (s *CommentService) ValidateURL(url string) bool {
	if url == "" {
		return true
	}
	return strings.HasPrefix(url, "http://") || strings.HasPrefix(url, "https://")
}
