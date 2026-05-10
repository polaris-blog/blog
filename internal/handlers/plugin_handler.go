package handlers

import (
	"os"
	"strconv"

	"github.com/gofiber/fiber/v2"
	"github.com/polaris/blog/internal/services"
	"go.uber.org/zap"
)

type PluginHandler struct {
	pluginService *services.PluginService
	logger        *zap.Logger
}

func NewPluginHandler(pluginService *services.PluginService, logger *zap.Logger) *PluginHandler {
	return &PluginHandler{pluginService: pluginService, logger: logger}
}

func (h *PluginHandler) List(c *fiber.Ctx) error {
	plugins, err := h.pluginService.List()
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "获取插件列表失败"})
	}
	return c.JSON(plugins)
}

func (h *PluginHandler) Upload(c *fiber.Ctx) error {
	file, err := c.FormFile("file")
	if err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "请选择要上传的插件包"})
	}

	tempPath := "/tmp/plugin_" + file.Filename
	if err := c.SaveFile(file, tempPath); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "保存文件失败"})
	}
	defer os.Remove(tempPath)

	plugin, err := h.pluginService.Upload(tempPath)
	if err != nil {
		h.logger.Error("Plugin upload failed", zap.Error(err))
		return c.Status(400).JSON(fiber.Map{"error": err.Error()})
	}

	return c.JSON(plugin)
}

func (h *PluginHandler) Activate(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	if err := h.pluginService.Activate(uint(id)); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "激活失败"})
	}

	return c.JSON(fiber.Map{"message": "插件已激活"})
}

func (h *PluginHandler) Deactivate(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	if err := h.pluginService.Deactivate(uint(id)); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "禁用失败"})
	}

	return c.JSON(fiber.Map{"message": "插件已禁用"})
}

func (h *PluginHandler) Delete(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	if err := h.pluginService.Delete(uint(id)); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": err.Error()})
	}

	return c.JSON(fiber.Map{"message": "删除成功"})
}

func (h *PluginHandler) GetConfig(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))
	plugin, err := h.pluginService.GetByID(uint(id))
	if err != nil {
		return c.Status(404).JSON(fiber.Map{"error": "插件不存在"})
	}

	resolved := h.pluginService.GetResolvedConfig(plugin)
	return c.JSON(resolved)
}

func (h *PluginHandler) UpdateConfig(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	var config map[string]string
	if err := c.BodyParser(&config); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	if err := h.pluginService.UpdateConfig(uint(id), config); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "更新配置失败"})
	}

	return c.JSON(fiber.Map{"message": "配置已更新"})
}

func (h *PluginHandler) ApprovePermissions(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	if err := h.pluginService.ApprovePermissions(uint(id)); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "审批失败"})
	}

	return c.JSON(fiber.Map{"message": "权限已审批"})
}
