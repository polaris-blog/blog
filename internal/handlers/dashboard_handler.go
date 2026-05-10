package handlers

import (
	"github.com/gofiber/fiber/v2"
	"github.com/polaris/blog/internal/models"
	"gorm.io/gorm"
)

type DashboardHandler struct {
	db *gorm.DB
}

func NewDashboardHandler(db *gorm.DB) *DashboardHandler {
	return &DashboardHandler{db: db}
}

type DashboardStats struct {
	TotalPosts      int64 `json:"total_posts"`
	PublishedPosts  int64 `json:"published_posts"`
	DraftPosts      int64 `json:"draft_posts"`
	TotalComments   int64 `json:"total_comments"`
	PendingComments int64 `json:"pending_comments"`
	TotalViews      int64 `json:"total_views"`
	TotalMedia      int64 `json:"total_media"`
	StorageUsed     int64 `json:"storage_used"`
}

func (h *DashboardHandler) Stats(c *fiber.Ctx) error {
	var stats DashboardStats

	h.db.Model(&models.Post{}).Count(&stats.TotalPosts)
	h.db.Model(&models.Post{}).Where("status = ?", models.PostStatusPublished).Count(&stats.PublishedPosts)
	h.db.Model(&models.Post{}).Where("status = ?", models.PostStatusDraft).Count(&stats.DraftPosts)

	h.db.Model(&models.Comment{}).Count(&stats.TotalComments)
	h.db.Model(&models.Comment{}).Where("status = ?", models.CommentStatusPending).Count(&stats.PendingComments)

	h.db.Model(&models.Post{}).Select("COALESCE(SUM(view_count), 0)").Scan(&stats.TotalViews)

	h.db.Model(&models.Media{}).Count(&stats.TotalMedia)
	h.db.Model(&models.Media{}).Select("COALESCE(SUM(size), 0)").Scan(&stats.StorageUsed)

	return c.JSON(stats)
}
