package handlers

import (
	"os"
	"strconv"

	"github.com/gofiber/fiber/v2"
	"github.com/polaris/blog/internal/services"
)

type ThemeHandler struct {
	themeService    *services.ThemeService
	onThemeActivate func() // callback to reload template engine
}

func NewThemeHandler(themeService *services.ThemeService, onActivate func()) *ThemeHandler {
	return &ThemeHandler{
		themeService:    themeService,
		onThemeActivate: onActivate,
	}
}

func (h *ThemeHandler) List(c *fiber.Ctx) error {
	themes, err := h.themeService.List()
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "获取主题列表失败"})
	}
	return c.JSON(themes)
}

func (h *ThemeHandler) Upload(c *fiber.Ctx) error {
	file, err := c.FormFile("file")
	if err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "请选择要上传的主题包"})
	}

	tempPath := "/tmp/theme_" + file.Filename
	if err := c.SaveFile(file, tempPath); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "保存文件失败"})
	}
	defer os.Remove(tempPath)

	theme, err := h.themeService.Upload(tempPath)
	if err != nil {
		return c.Status(400).JSON(fiber.Map{"error": err.Error()})
	}

	return c.JSON(theme)
}

func (h *ThemeHandler) Activate(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	if err := h.themeService.Activate(uint(id)); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "激活失败"})
	}

	// Reload template engine with the new active theme
	if h.onThemeActivate != nil {
		h.onThemeActivate()
	}

	return c.JSON(fiber.Map{"message": "主题已激活"})
}

func (h *ThemeHandler) Delete(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	if err := h.themeService.Delete(uint(id)); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": err.Error()})
	}

	return c.JSON(fiber.Map{"message": "删除成功"})
}

func (h *ThemeHandler) Preview(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	path, err := h.themeService.Preview(uint(id))
	if err != nil {
		return c.Status(404).JSON(fiber.Map{"error": "主题不存在"})
	}

	return c.JSON(fiber.Map{"path": path})
}

func (h *ThemeHandler) GetConfig(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))
	theme, err := h.themeService.GetByID(uint(id))
	if err != nil {
		return c.Status(404).JSON(fiber.Map{"error": "主题不存在"})
	}

	resolved := h.themeService.GetResolvedConfig(theme)
	return c.JSON(resolved)
}

func (h *ThemeHandler) UpdateConfig(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	var config map[string]string
	if err := c.BodyParser(&config); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	if err := h.themeService.UpdateConfig(uint(id), config); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "更新配置失败"})
	}

	// Reload theme engine so config changes take effect immediately
	if h.onThemeActivate != nil {
		h.onThemeActivate()
	}

	return c.JSON(fiber.Map{"message": "配置已更新"})
}
