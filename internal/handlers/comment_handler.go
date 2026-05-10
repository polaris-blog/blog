package handlers

import (
	"html"
	"regexp"
	"strconv"
	"strings"

	"github.com/gofiber/fiber/v2"
	"github.com/polaris/blog/internal/models"
	"github.com/polaris/blog/internal/services"
)

type CommentHandler struct {
	commentService *services.CommentService
}

func NewCommentHandler(commentService *services.CommentService) *CommentHandler {
	return &CommentHandler{commentService: commentService}
}

func (h *CommentHandler) List(c *fiber.Ctx) error {
	page, _ := strconv.Atoi(c.Query("page", "1"))
	pageSize, _ := strconv.Atoi(c.Query("page_size", "10"))
	postID, _ := strconv.Atoi(c.Query("post_id"))

	params := services.CommentListParams{
		Page:     page,
		PageSize: pageSize,
		PostID:   uint(postID),
		Status:   models.CommentStatusApproved,
	}

	comments, total, err := h.commentService.List(params)
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "获取评论列表失败"})
	}

	return c.JSON(fiber.Map{
		"comments": comments,
		"total":    total,
		"page":     page,
	})
}

func (h *CommentHandler) Create(c *fiber.Ctx) error {
	var comment models.Comment
	if err := c.BodyParser(&comment); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	if comment.AuthorName == "" || comment.AuthorEmail == "" {
		return c.Status(400).JSON(fiber.Map{"error": "昵称和邮箱不能为空"})
	}

	// Input length limits
	if len(comment.AuthorName) > 50 {
		return c.Status(400).JSON(fiber.Map{"error": "昵称长度不能超过50个字符"})
	}
	if len(comment.AuthorEmail) > 100 {
		return c.Status(400).JSON(fiber.Map{"error": "邮箱长度不能超过100个字符"})
	}
	if len(comment.Content) > 2000 {
		return c.Status(400).JSON(fiber.Map{"error": "评论内容不能超过2000个字符"})
	}
	if len(comment.Content) < 1 {
		return c.Status(400).JSON(fiber.Map{"error": "评论内容不能为空"})
	}
	if comment.AuthorURL != "" && len(comment.AuthorURL) > 200 {
		return c.Status(400).JSON(fiber.Map{"error": "网站地址过长"})
	}

	if !h.commentService.ValidateEmail(comment.AuthorEmail) {
		return c.Status(400).JSON(fiber.Map{"error": "请输入正确的邮箱地址"})
	}

	// Sanitize inputs: strip HTML tags, trim whitespace
	comment.AuthorName = strings.TrimSpace(html.EscapeString(stripTags(comment.AuthorName)))
	comment.AuthorEmail = strings.TrimSpace(comment.AuthorEmail)
	comment.Content = strings.TrimSpace(stripTags(comment.Content))
	if comment.AuthorURL != "" {
		comment.AuthorURL = strings.TrimSpace(comment.AuthorURL)
	}

	if comment.Content == "" {
		return c.Status(400).JSON(fiber.Map{"error": "评论内容不能为空"})
	}

	comment.IP = c.IP()
	comment.UserAgent = c.Get("User-Agent")

	moderationMode := "post"
	if err := h.commentService.Create(&comment, moderationMode); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "发表评论失败"})
	}

	return c.JSON(comment)
}

// stripTags removes all HTML tags from a string.
func stripTags(s string) string {
	re := regexp.MustCompile(`<[^>]*>`)
	return re.ReplaceAllString(s, "")
}

func (h *CommentHandler) Like(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	ip := c.IP()
	if err := h.commentService.Like(uint(id), ip); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": err.Error()})
	}

	return c.JSON(fiber.Map{"message": "点赞成功"})
}

func (h *CommentHandler) AdminList(c *fiber.Ctx) error {
	page, _ := strconv.Atoi(c.Query("page", "1"))
	pageSize, _ := strconv.Atoi(c.Query("page_size", "10"))
	postID, _ := strconv.Atoi(c.Query("post_id"))

	params := services.CommentListParams{
		Page:     page,
		PageSize: pageSize,
		PostID:   uint(postID),
		Status:   c.Query("status"),
	}

	comments, total, err := h.commentService.AdminList(params)
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "获取评论列表失败"})
	}

	return c.JSON(fiber.Map{
		"comments": comments,
		"total":    total,
		"page":     page,
	})
}

func (h *CommentHandler) Approve(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	if err := h.commentService.Approve(uint(id)); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "审核失败"})
	}

	return c.JSON(fiber.Map{"message": "审核通过"})
}

func (h *CommentHandler) MarkSpam(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	if err := h.commentService.MarkSpam(uint(id)); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "标记失败"})
	}

	return c.JSON(fiber.Map{"message": "已标记为垃圾评论"})
}

func (h *CommentHandler) Delete(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	if err := h.commentService.Delete(uint(id)); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "删除失败"})
	}

	return c.JSON(fiber.Map{"message": "删除成功"})
}

func (h *CommentHandler) BatchAction(c *fiber.Ctx) error {
	var req struct {
		Action string `json:"action"`
		IDs    []uint `json:"ids"`
	}

	if err := c.BodyParser(&req); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	if len(req.IDs) == 0 {
		return c.Status(400).JSON(fiber.Map{"error": "请选择评论"})
	}

	if err := h.commentService.BatchAction(req.Action, req.IDs); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": err.Error()})
	}

	return c.JSON(fiber.Map{"message": "操作成功"})
}
