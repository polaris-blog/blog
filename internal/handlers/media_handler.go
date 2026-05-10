package handlers

import (
	"fmt"
	"mime/multipart"
	"path/filepath"
	"strconv"
	"strings"

	"github.com/gofiber/fiber/v2"
	"github.com/polaris/blog/internal/services"
)

// allowedImageExts are the only file extensions allowed for media uploads.
var allowedImageExts = map[string]bool{
	".jpg": true, ".jpeg": true, ".png": true, ".gif": true,
	".webp": true, ".svg": true, ".ico": true, ".bmp": true,
}

const maxUploadSize = 10 << 20 // 10MB

type MediaHandler struct {
	mediaService *services.MediaService
}

func NewMediaHandler(mediaService *services.MediaService) *MediaHandler {
	return &MediaHandler{mediaService: mediaService}
}

func (h *MediaHandler) List(c *fiber.Ctx) error {
	page, _ := strconv.Atoi(c.Query("page", "1"))
	pageSize, _ := strconv.Atoi(c.Query("page_size", "20"))

	params := services.MediaListParams{
		Page:     page,
		PageSize: pageSize,
		Type:     c.Query("type"),
		Keyword:  c.Query("keyword"),
	}

	media, total, err := h.mediaService.List(params)
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "获取媒体列表失败"})
	}

	return c.JSON(fiber.Map{
		"media": media,
		"total": total,
		"page":  page,
	})
}

func (h *MediaHandler) Upload(c *fiber.Ctx) error {
	file, err := c.FormFile("file")
	if err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "请选择要上传的文件"})
	}

	if err := validateUpload(file); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": err.Error()})
	}

	media, err := h.mediaService.Upload(file)
	if err != nil {
		return c.Status(400).JSON(fiber.Map{"error": err.Error()})
	}

	return c.JSON(media)
}

func (h *MediaHandler) UploadLogo(c *fiber.Ctx) error {
	file, err := c.FormFile("file")
	if err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "请选择要上传的文件"})
	}

	if err := validateUpload(file); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": err.Error()})
	}

	media, err := h.mediaService.Upload(file)
	if err != nil {
		return c.Status(400).JSON(fiber.Map{"error": err.Error()})
	}

	return c.JSON(fiber.Map{"url": media.URL})
}

func (h *MediaHandler) UploadFavicon(c *fiber.Ctx) error {
	file, err := c.FormFile("file")
	if err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "请选择要上传的文件"})
	}

	if err := validateUpload(file); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": err.Error()})
	}

	media, err := h.mediaService.Upload(file)
	if err != nil {
		return c.Status(400).JSON(fiber.Map{"error": err.Error()})
	}

	return c.JSON(fiber.Map{"url": media.URL})
}

func (h *MediaHandler) Delete(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	if err := h.mediaService.Delete(uint(id)); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "删除失败"})
	}

	return c.JSON(fiber.Map{"message": "删除成功"})
}

// validateUpload checks file size and extension whitelist.
func validateUpload(file *multipart.FileHeader) error {
	if file.Size > maxUploadSize {
		return fmt.Errorf("文件大小不能超过%dMB", maxUploadSize/(1<<20))
	}

	ext := strings.ToLower(filepath.Ext(file.Filename))
	if !allowedImageExts[ext] {
		return fmt.Errorf("不支持的文件类型: %s，允许的类型: jpg, png, gif, webp, svg, ico", ext)
	}

	return nil
}
