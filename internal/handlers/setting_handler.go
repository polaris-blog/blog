package handlers

import (
	"github.com/gofiber/fiber/v2"
	"github.com/polaris/blog/internal/models"
	"github.com/polaris/blog/internal/services"
)

type SettingHandler struct {
	settingService *services.SettingService
}

func NewSettingHandler(settingService *services.SettingService) *SettingHandler {
	return &SettingHandler{settingService: settingService}
}

func (h *SettingHandler) GetPublic(c *fiber.Ctx) error {
	settings, err := h.settingService.GetPublic()
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "获取设置失败"})
	}
	return c.JSON(settings)
}

func (h *SettingHandler) GetAll(c *fiber.Ctx) error {
	settings, err := h.settingService.GetAll()
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "获取设置失败"})
	}
	return c.JSON(settings)
}

func (h *SettingHandler) Update(c *fiber.Ctx) error {
	var settings map[string]string
	if err := c.BodyParser(&settings); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	if err := h.settingService.Update(settings); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "更新设置失败"})
	}

	return c.JSON(fiber.Map{"message": "设置已保存"})
}

func (h *SettingHandler) ListFriendLinks(c *fiber.Ctx) error {
	links, err := h.settingService.GetAllFriendLinks()
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "获取友情链接失败"})
	}
	return c.JSON(links)
}

func (h *SettingHandler) CreateFriendLink(c *fiber.Ctx) error {
	var link models.FriendLink
	if err := c.BodyParser(&link); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	if link.Name == "" || link.URL == "" {
		return c.Status(400).JSON(fiber.Map{"error": "名称和URL不能为空"})
	}

	if err := h.settingService.CreateFriendLink(&link); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "创建友情链接失败"})
	}

	return c.JSON(link)
}

func (h *SettingHandler) UpdateFriendLink(c *fiber.Ctx) error {
	id, _ := c.ParamsInt("id")

	var link models.FriendLink
	if err := c.BodyParser(&link); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	link.ID = uint(id)

	if err := h.settingService.UpdateFriendLink(&link); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "更新友情链接失败"})
	}

	return c.JSON(link)
}

func (h *SettingHandler) DeleteFriendLink(c *fiber.Ctx) error {
	id, _ := c.ParamsInt("id")

	if err := h.settingService.DeleteFriendLink(uint(id)); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "删除友情链接失败"})
	}

	return c.JSON(fiber.Map{"message": "删除成功"})
}
