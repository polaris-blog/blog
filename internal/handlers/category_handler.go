package handlers

import (
	"strconv"

	"github.com/gofiber/fiber/v2"
	"github.com/polaris/blog/internal/models"
	"github.com/polaris/blog/internal/services"
)

type CategoryHandler struct {
	categoryService *services.CategoryService
}

func NewCategoryHandler(categoryService *services.CategoryService) *CategoryHandler {
	return &CategoryHandler{categoryService: categoryService}
}

func (h *CategoryHandler) List(c *fiber.Ctx) error {
	categories, err := h.categoryService.List()
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "获取分类列表失败"})
	}
	return c.JSON(categories)
}

func (h *CategoryHandler) Get(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	category, err := h.categoryService.GetByID(uint(id))
	if err != nil {
		return c.Status(404).JSON(fiber.Map{"error": "分类不存在"})
	}

	return c.JSON(category)
}

func (h *CategoryHandler) AdminList(c *fiber.Ctx) error {
	categories, err := h.categoryService.AdminList()
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "获取分类列表失败"})
	}
	return c.JSON(categories)
}

func (h *CategoryHandler) Create(c *fiber.Ctx) error {
	var category models.Category
	if err := c.BodyParser(&category); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	if category.Name == "" {
		return c.Status(400).JSON(fiber.Map{"error": "分类名称不能为空"})
	}

	if err := h.categoryService.Create(&category); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "创建分类失败"})
	}

	return c.JSON(category)
}

func (h *CategoryHandler) Update(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	category, err := h.categoryService.GetByID(uint(id))
	if err != nil {
		return c.Status(404).JSON(fiber.Map{"error": "分类不存在"})
	}

	if err := c.BodyParser(category); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	if err := h.categoryService.Update(category); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "更新分类失败"})
	}

	return c.JSON(category)
}

func (h *CategoryHandler) Delete(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	if err := h.categoryService.Delete(uint(id)); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "删除分类失败"})
	}

	return c.JSON(fiber.Map{"message": "删除成功"})
}
