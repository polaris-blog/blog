package handlers

import (
	"strconv"

	"github.com/gofiber/fiber/v2"
	"github.com/polaris/blog/internal/models"
	"github.com/polaris/blog/internal/services"
)

type NavigationHandler struct {
	navService *services.NavigationService
}

func NewNavigationHandler(navService *services.NavigationService) *NavigationHandler {
	return &NavigationHandler{navService: navService}
}

func (h *NavigationHandler) List(c *fiber.Ctx) error {
	navs, err := h.navService.GetAll()
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "获取导航失败"})
	}
	return c.JSON(navs)
}

func (h *NavigationHandler) PublicList(c *fiber.Ctx) error {
	navs, err := h.navService.GetActive()
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "获取导航失败"})
	}
	return c.JSON(navs)
}

func (h *NavigationHandler) Create(c *fiber.Ctx) error {
	var req struct {
		Name      string `json:"name"`
		URL       string `json:"url"`
		Icon      string `json:"icon"`
		Target    string `json:"target"`
		SortOrder int    `json:"sort_order"`
		ParentID  *uint  `json:"parent_id"`
		IsActive  bool   `json:"is_active"`
	}
	if err := c.BodyParser(&req); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	nav := &models.Navigation{
		Name:      req.Name,
		URL:       req.URL,
		Icon:      req.Icon,
		Target:    req.Target,
		SortOrder: req.SortOrder,
		ParentID:  req.ParentID,
		IsActive:  req.IsActive,
	}

	if err := h.navService.Create(nav); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "创建导航失败"})
	}

	return c.JSON(fiber.Map{"id": nav.ID, "message": "创建成功"})
}

func (h *NavigationHandler) Update(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	var req struct {
		Name      string `json:"name"`
		URL       string `json:"url"`
		Icon      string `json:"icon"`
		Target    string `json:"target"`
		SortOrder int    `json:"sort_order"`
		ParentID  *uint  `json:"parent_id"`
		IsActive  bool   `json:"is_active"`
	}
	if err := c.BodyParser(&req); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	nav, err := h.navService.GetByID(uint(id))
	if err != nil {
		return c.Status(404).JSON(fiber.Map{"error": "导航不存在"})
	}

	nav.Name = req.Name
	nav.URL = req.URL
	nav.Icon = req.Icon
	nav.Target = req.Target
	nav.SortOrder = req.SortOrder
	nav.ParentID = req.ParentID
	nav.IsActive = req.IsActive

	if err := h.navService.Update(nav); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "更新导航失败"})
	}

	return c.JSON(fiber.Map{"message": "更新成功"})
}

func (h *NavigationHandler) Delete(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	if err := h.navService.Delete(uint(id)); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "删除导航失败"})
	}

	return c.JSON(fiber.Map{"message": "删除成功"})
}

func (h *NavigationHandler) UpdateOrder(c *fiber.Ctx) error {
	var req struct {
		Items []struct {
			ID        uint  `json:"id"`
			SortOrder int   `json:"sort_order"`
			ParentID  *uint `json:"parent_id"`
		} `json:"items"`
	}
	if err := c.BodyParser(&req); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	if err := h.navService.UpdateOrder(req.Items); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "更新排序失败"})
	}

	return c.JSON(fiber.Map{"message": "排序更新成功"})
}
