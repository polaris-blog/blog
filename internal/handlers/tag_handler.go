package handlers

import (
	"strconv"
	"strings"

	"github.com/gofiber/fiber/v2"
	"github.com/polaris/blog/internal/services"
)

type TagHandler struct {
	tagService *services.TagService
}

func NewTagHandler(tagService *services.TagService) *TagHandler {
	return &TagHandler{tagService: tagService}
}

func (h *TagHandler) List(c *fiber.Ctx) error {
	tags, err := h.tagService.List()
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "获取标签列表失败"})
	}
	return c.JSON(tags)
}

func (h *TagHandler) Get(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	tag, err := h.tagService.GetByID(uint(id))
	if err != nil {
		return c.Status(404).JSON(fiber.Map{"error": "标签不存在"})
	}

	return c.JSON(tag)
}

func (h *TagHandler) AdminList(c *fiber.Ctx) error {
	tags, err := h.tagService.AdminList()
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "获取标签列表失败"})
	}
	return c.JSON(tags)
}

func (h *TagHandler) Create(c *fiber.Ctx) error {
	var req struct {
		Name string `json:"name"`
	}
	if err := c.BodyParser(&req); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	if req.Name == "" {
		return c.Status(400).JSON(fiber.Map{"error": "标签名称不能为空"})
	}

	tag, err := h.tagService.GetOrCreate(req.Name)
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "创建标签失败"})
	}

	if tag.Slug == "" {
		tag.Slug = strings.ToLower(strings.ReplaceAll(tag.Name, " ", "-"))
		h.tagService.Update(tag)
	}

	return c.JSON(fiber.Map{
		"id":        tag.ID,
		"name":      tag.Name,
		"slug":      tag.Slug,
		"post_count": tag.PostCount,
	})
}

func (h *TagHandler) Update(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	tag, err := h.tagService.GetByID(uint(id))
	if err != nil {
		return c.Status(404).JSON(fiber.Map{"error": "标签不存在"})
	}

	if err := c.BodyParser(tag); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	if err := h.tagService.Update(tag); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "更新标签失败"})
	}

	return c.JSON(tag)
}

func (h *TagHandler) Delete(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	if err := h.tagService.Delete(uint(id)); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "删除标签失败"})
	}

	return c.JSON(fiber.Map{"message": "删除成功"})
}

func (h *TagHandler) Merge(c *fiber.Ctx) error {
	var req struct {
		SourceID uint `json:"source_id"`
		TargetID uint `json:"target_id"`
	}

	if err := c.BodyParser(&req); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	if req.SourceID == 0 || req.TargetID == 0 {
		return c.Status(400).JSON(fiber.Map{"error": "请选择源标签和目标标签"})
	}

	if err := h.tagService.Merge(req.SourceID, req.TargetID); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": err.Error()})
	}

	return c.JSON(fiber.Map{"message": "合并成功"})
}
