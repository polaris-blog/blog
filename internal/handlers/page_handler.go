package handlers

import (
	"strconv"

	"github.com/gofiber/fiber/v2"
	"github.com/polaris/blog/internal/models"
	"github.com/polaris/blog/internal/services"
)

type PageHandler struct {
	pageService *services.PageService
}

func NewPageHandler(pageService *services.PageService) *PageHandler {
	return &PageHandler{pageService: pageService}
}

func (h *PageHandler) List(c *fiber.Ctx) error {
	page, _ := strconv.Atoi(c.Query("page", "1"))
	pageSize, _ := strconv.Atoi(c.Query("page_size", "10"))

	params := services.PageListParams{
		Page:     page,
		PageSize: pageSize,
		Status:   c.Query("status"),
		Keyword:  c.Query("keyword"),
	}

	pages, total, err := h.pageService.List(params)
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "获取页面列表失败"})
	}

	return c.JSON(fiber.Map{
		"pages": pages,
		"total": total,
		"page":  page,
	})
}

func (h *PageHandler) Get(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	page, err := h.pageService.GetByID(uint(id))
	if err != nil {
		return c.Status(404).JSON(fiber.Map{"error": "页面不存在"})
	}

	return c.JSON(page)
}

func (h *PageHandler) GetBySlug(c *fiber.Ctx) error {
	slug := c.Params("slug")

	page, err := h.pageService.GetBySlug(slug)
	if err != nil {
		return c.Status(404).JSON(fiber.Map{"error": "页面不存在"})
	}

	return c.JSON(page)
}

func (h *PageHandler) Create(c *fiber.Ctx) error {
	var req struct {
		Title      string `json:"title"`
		Slug       string `json:"slug"`
		Content    string `json:"content"`
		Excerpt    string `json:"excerpt"`
		CoverImage string `json:"cover_image"`
		Status     string `json:"status"`
		IsInNav    bool   `json:"is_in_nav"`
		NavSort    int    `json:"nav_sort"`
	}
	if err := c.BodyParser(&req); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	userID := c.Locals("userID").(uint)
	page := &models.Page{
		Title:      req.Title,
		Slug:       req.Slug,
		Content:    req.Content,
		Excerpt:    req.Excerpt,
		CoverImage: req.CoverImage,
		Status:     req.Status,
		IsInNav:    req.IsInNav,
		NavSort:    req.NavSort,
		AuthorID:   userID,
	}

	if err := h.pageService.Create(page); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "创建页面失败"})
	}

	return c.JSON(fiber.Map{"id": page.ID, "message": "创建成功"})
}

func (h *PageHandler) Update(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	page, err := h.pageService.GetByID(uint(id))
	if err != nil {
		return c.Status(404).JSON(fiber.Map{"error": "页面不存在"})
	}

	var req struct {
		Title      string `json:"title"`
		Slug       string `json:"slug"`
		Content    string `json:"content"`
		Excerpt    string `json:"excerpt"`
		CoverImage string `json:"cover_image"`
		Status     string `json:"status"`
		IsInNav    bool   `json:"is_in_nav"`
		NavSort    int    `json:"nav_sort"`
	}
	if err := c.BodyParser(&req); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	page.Title = req.Title
	page.Slug = req.Slug
	page.Content = req.Content
	page.Excerpt = req.Excerpt
	page.CoverImage = req.CoverImage
	page.Status = req.Status
	page.IsInNav = req.IsInNav
	page.NavSort = req.NavSort

	if err := h.pageService.Update(page); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "更新页面失败"})
	}

	return c.JSON(fiber.Map{"message": "更新成功"})
}

func (h *PageHandler) Delete(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	if err := h.pageService.Delete(uint(id)); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "删除页面失败"})
	}

	return c.JSON(fiber.Map{"message": "删除成功"})
}

func (h *PageHandler) Publish(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	if err := h.pageService.Publish(uint(id)); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "发布失败"})
	}

	return c.JSON(fiber.Map{"message": "发布成功"})
}

func (h *PageHandler) GetNavPages(c *fiber.Ctx) error {
	pages, err := h.pageService.GetNavPages()
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "获取导航页面失败"})
	}
	return c.JSON(pages)
}
