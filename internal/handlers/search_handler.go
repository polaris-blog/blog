package handlers

import (
	"strconv"

	"github.com/gofiber/fiber/v2"
	"github.com/polaris/blog/internal/services"
)

type SearchHandler struct {
	searchService *services.SearchService
}

func NewSearchHandler(searchService *services.SearchService) *SearchHandler {
	return &SearchHandler{searchService: searchService}
}

func (h *SearchHandler) Search(c *fiber.Ctx) error {
	keyword := c.Query("q")
	if len(keyword) < 2 {
		return c.Status(400).JSON(fiber.Map{"error": "请输入至少 2 个字符"})
	}

	page, _ := strconv.Atoi(c.Query("page", "1"))
	pageSize, _ := strconv.Atoi(c.Query("page_size", "10"))

	params := services.SearchParams{
		Keyword:  keyword,
		Page:     page,
		PageSize: pageSize,
	}

	result, err := h.searchService.Search(params)
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "搜索失败"})
	}

	for i, post := range result.Posts {
		result.Posts[i].Content = h.searchService.GetExcerpt(post.Content, keyword, 200)
	}

	return c.JSON(fiber.Map{
		"posts": result.Posts,
		"total": result.Total,
		"keyword": keyword,
		"page": page,
	})
}
