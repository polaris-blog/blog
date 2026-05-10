package handlers

import (
	"bytes"
	"strconv"
	"time"

	"github.com/gofiber/fiber/v2"
	"github.com/polaris/blog/internal/models"
	"github.com/polaris/blog/internal/services"
	"text/template"
)

type PostHandler struct {
	postService    *services.PostService
	settingService *services.SettingService
	pageService    *services.PageService
	categoryService *services.CategoryService
	tagService     *services.TagService
}

func NewPostHandler(postService *services.PostService, settingService *services.SettingService, pageService *services.PageService, categoryService *services.CategoryService, tagService *services.TagService) *PostHandler {
	return &PostHandler{
		postService:    postService,
		settingService: settingService,
		pageService:    pageService,
		categoryService: categoryService,
		tagService:     tagService,
	}
}

func (h *PostHandler) List(c *fiber.Ctx) error {
	page, _ := strconv.Atoi(c.Query("page", "1"))
	pageSize, _ := strconv.Atoi(c.Query("page_size", "10"))
	categoryID, _ := strconv.Atoi(c.Query("category"))
	tagID, _ := strconv.Atoi(c.Query("tag"))

	params := services.PostListParams{
		Page:     page,
		PageSize: pageSize,
		Status:   models.PostStatusPublished,
		Category: uint(categoryID),
		Tag:      uint(tagID),
		Keyword:  c.Query("keyword"),
	}

	posts, total, err := h.postService.List(params)
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "获取文章列表失败"})
	}

	return c.JSON(fiber.Map{
		"posts": posts,
		"total": total,
		"page":  page,
	})
}

func (h *PostHandler) Get(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	post, err := h.postService.GetByID(uint(id))
	if err != nil {
		return c.Status(404).JSON(fiber.Map{"error": "文章不存在"})
	}

	if post.Status != models.PostStatusPublished {
		return c.Status(404).JSON(fiber.Map{"error": "文章不存在"})
	}

	if post.Password != "" {
		password := c.Query("password")
		if password == "" || !h.postService.CheckPassword(post, password) {
			return c.JSON(fiber.Map{
				"need_password": true,
				"id":            post.ID,
				"title":         post.Title,
			})
		}
	}

	return c.JSON(post)
}

func (h *PostHandler) GetBySlug(c *fiber.Ctx) error {
	slug := c.Params("slug")

	post, err := h.postService.GetBySlug(slug)
	if err != nil {
		return c.Status(404).JSON(fiber.Map{"error": "文章不存在"})
	}

	if post.Password != "" {
		password := c.Query("password")
		if password == "" || !h.postService.CheckPassword(post, password) {
			return c.JSON(fiber.Map{
				"need_password": true,
				"id":            post.ID,
				"title":         post.Title,
			})
		}
	}

	return c.JSON(post)
}

func (h *PostHandler) AdminList(c *fiber.Ctx) error {
	page, _ := strconv.Atoi(c.Query("page", "1"))
	pageSize, _ := strconv.Atoi(c.Query("page_size", "10"))
	categoryID, _ := strconv.Atoi(c.Query("category"))

	params := services.PostListParams{
		Page:     page,
		PageSize: pageSize,
		Status:   c.Query("status"),
		Category: uint(categoryID),
		Keyword:  c.Query("keyword"),
	}

	posts, total, err := h.postService.AdminList(params)
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "获取文章列表失败"})
	}

	return c.JSON(fiber.Map{
		"posts": posts,
		"total": total,
		"page":  page,
	})
}

func (h *PostHandler) Create(c *fiber.Ctx) error {
	var req struct {
		Title      string `json:"title"`
		Content    string `json:"content"`
		Excerpt    string `json:"excerpt"`
		CoverImage string `json:"cover_image"`
		Status     string `json:"status"`
		CategoryID *uint  `json:"category_id"`
		TagIDs     []uint `json:"tag_ids"`
	}
	if err := c.BodyParser(&req); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	userID := c.Locals("userID").(uint)
	post := &models.Post{
		Title:      req.Title,
		Content:    req.Content,
		Excerpt:    req.Excerpt,
		CoverImage: req.CoverImage,
		Status:     req.Status,
		CategoryID: req.CategoryID,
		AuthorID:   userID,
	}

	if err := h.postService.Create(post, req.TagIDs); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "创建文章失败"})
	}

	return c.JSON(fiber.Map{
		"id":      post.ID,
		"message": "创建成功",
	})
}

func (h *PostHandler) Update(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	post, err := h.postService.GetByIDAdmin(uint(id))
	if err != nil {
		return c.Status(404).JSON(fiber.Map{"error": "文章不存在"})
	}

	var req struct {
		Title      string `json:"title"`
		Content    string `json:"content"`
		Excerpt    string `json:"excerpt"`
		CoverImage string `json:"cover_image"`
		Status     string `json:"status"`
		CategoryID *uint  `json:"category_id"`
		TagIDs     []uint `json:"tag_ids"`
	}
	if err := c.BodyParser(&req); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	post.Title = req.Title
	post.Content = req.Content
	post.Excerpt = req.Excerpt
	post.CoverImage = req.CoverImage
	post.Status = req.Status
	post.CategoryID = req.CategoryID

	if err := h.postService.Update(post, req.TagIDs); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "更新文章失败"})
	}

	return c.JSON(fiber.Map{
		"id":      post.ID,
		"message": "更新成功",
	})
}

func (h *PostHandler) Delete(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	if err := h.postService.Delete(uint(id)); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "删除文章失败"})
	}

	return c.JSON(fiber.Map{"message": "删除成功"})
}

func (h *PostHandler) Publish(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	if err := h.postService.Publish(uint(id)); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "发布失败"})
	}

	return c.JSON(fiber.Map{"message": "发布成功"})
}

func (h *PostHandler) Schedule(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	var req struct {
		PublishAt time.Time `json:"publish_at"`
	}

	if err := c.BodyParser(&req); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	if req.PublishAt.Before(time.Now()) {
		return c.Status(400).JSON(fiber.Map{"error": "定时时间必须大于当前时间"})
	}

	if err := h.postService.Schedule(uint(id), req.PublishAt); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "定时发布设置失败"})
	}

	return c.JSON(fiber.Map{"message": "定时发布设置成功"})
}

func (h *PostHandler) Versions(c *fiber.Ctx) error {
	id, _ := strconv.Atoi(c.Params("id"))

	versions, err := h.postService.GetVersions(uint(id))
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "获取版本历史失败"})
	}

	return c.JSON(versions)
}

const rssTemplate = `<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"
  xmlns:atom="http://www.w3.org/2005/Atom"
  xmlns:dc="http://purl.org/dc/elements/1.1/"
  xmlns:content="http://purl.org/rss/1.0/modules/content/">
  <channel>
    <title>{{if .Title}}{{.Title}}{{else}}Polaris Blog{{end}}</title>
    <link>{{.SiteURL}}</link>
    <description>{{if .Subtitle}}{{.Subtitle}}{{else}}{{.Title}} - RSS Feed{{end}}</description>
    <language>zh-CN</language>
    <lastBuildDate>{{.LastBuildDate}}</lastBuildDate>
    <atom:link href="{{.SiteURL}}/rss" rel="self" type="application/rss+xml"/>
    <generator>Polaris Blog</generator>
    {{if .AuthorName}}<managingEditor>{{.AuthorEmail}}</managingEditor>{{end}}
    {{range .Posts}}
    <item>
      <title>{{.Title}}</title>
      <link>{{$.SiteURL}}/post/{{.Slug}}</link>
      <guid isPermaLink="true">{{$.SiteURL}}/post/{{.Slug}}</guid>
      <pubDate>{{if .PublishedAt}}{{.PublishedAt.Format "Mon, 02 Jan 2006 15:04:05 -0700"}}{{else}}{{.CreatedAt.Format "Mon, 02 Jan 2006 15:04:05 -0700"}}{{end}}</pubDate>
      <dc:creator>{{if .Author.Nickname}}{{.Author.Nickname}}{{else}}{{.Author.Username}}{{end}}</dc:creator>
      {{if .Category}}<category>{{.Category.Name}}</category>{{end}}
      {{range .Tags}}<category>{{.Name}}</category>{{end}}
      <description>{{.Excerpt}}</description>
      {{if $.FullContent}}
      <content:encoded><![CDATA[{{.Content}}]]></content:encoded>
      {{end}}
    </item>
    {{end}}
    {{range .Pages}}
    <item>
      <title>{{.Title}}</title>
      <link>{{$.SiteURL}}/page/{{.Slug}}</link>
      <guid isPermaLink="true">{{$.SiteURL}}/page/{{.Slug}}</guid>
      <pubDate>{{if .UpdatedAt}}{{.UpdatedAt.Format "Mon, 02 Jan 2006 15:04:05 -0700"}}{{else}}{{.CreatedAt.Format "Mon, 02 Jan 2006 15:04:05 -0700"}}{{end}}</pubDate>
      <description>{{.Content}}</description>
    </item>
    {{end}}
  </channel>
</rss>
`

const sitemapTemplate = `<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <url>
    <loc>{{.SiteURL}}</loc>
    <lastmod>{{.LastMod}}</lastmod>
    <changefreq>{{.Changefreq}}</changefreq>
    <priority>1.0</priority>
  </url>
  {{range .Posts}}
  <url>
    <loc>{{$.SiteURL}}/post/{{.Slug}}</loc>
    <lastmod>{{if .UpdatedAt}}{{.UpdatedAt.Format "2006-01-02"}}{{else if .PublishedAt}}{{.PublishedAt.Format "2006-01-02"}}{{else}}{{.CreatedAt.Format "2006-01-02"}}{{end}}</lastmod>
    <changefreq>{{$.Changefreq}}</changefreq>
    <priority>{{$.Priority}}</priority>
  </url>
  {{end}}
  {{range .Pages}}
  <url>
    <loc>{{$.SiteURL}}/page/{{.Slug}}</loc>
    <lastmod>{{if .UpdatedAt}}{{.UpdatedAt.Format "2006-01-02"}}{{else}}{{.CreatedAt.Format "2006-01-02"}}{{end}}</lastmod>
    <changefreq>monthly</changefreq>
    <priority>0.6</priority>
  </url>
  {{end}}
  {{range .Categories}}
  <url>
    <loc>{{$.SiteURL}}/category/{{.Slug}}</loc>
    <lastmod>{{$.LastMod}}</lastmod>
    <changefreq>weekly</changefreq>
    <priority>0.5</priority>
  </url>
  {{end}}
  {{range .Tags}}
  <url>
    <loc>{{$.SiteURL}}/tag/{{.Slug}}</loc>
    <lastmod>{{$.LastMod}}</lastmod>
    <changefreq>weekly</changefreq>
    <priority>0.4</priority>
  </url>
  {{end}}
  <url>
    <loc>{{.SiteURL}}/categories</loc>
    <lastmod>{{.LastMod}}</lastmod>
    <changefreq>weekly</changefreq>
    <priority>0.5</priority>
  </url>
  <url>
    <loc>{{.SiteURL}}/tags</loc>
    <lastmod>{{.LastMod}}</lastmod>
    <changefreq>weekly</changefreq>
    <priority>0.4</priority>
  </url>
  <url>
    <loc>{{.SiteURL}}/archives</loc>
    <lastmod>{{.LastMod}}</lastmod>
    <changefreq>daily</changefreq>
    <priority>0.3</priority>
  </url>
</urlset>
`

func (h *PostHandler) RSS(c *fiber.Ctx) error {
	settings, err := h.settingService.GetPublic()
	if err != nil {
		settings = make(map[string]string)
	}

	siteURL := settings["site_url"]
	if siteURL == "" {
		siteURL = c.Protocol() + "://" + c.Hostname()
	}

	posts, _, err := h.postService.List(services.PostListParams{
		Page:     1,
		PageSize: 20,
		Status:   models.PostStatusPublished,
	})
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "获取文章失败"})
	}

	pages, _, err := h.pageService.List(services.PageListParams{
		Page:     1,
		PageSize: 50,
		Status:   models.PageStatusPublished,
	})

	var lastBuildDate string
	if len(posts) > 0 {
		if posts[0].PublishedAt != nil {
			lastBuildDate = posts[0].PublishedAt.Format("Mon, 02 Jan 2006 15:04:05 -0700")
		} else {
			lastBuildDate = posts[0].CreatedAt.Format("Mon, 02 Jan 2006 15:04:05 -0700")
		}
	} else {
		lastBuildDate = time.Now().Format("Mon, 02 Jan 2006 15:04:05 -0700")
	}

	data := map[string]interface{}{
		"Posts":         posts,
		"Pages":         pages,
		"Title":         settings["site_title"],
		"Subtitle":      settings["site_subtitle"],
		"SiteURL":       siteURL,
		"AuthorName":    settings["author_name"],
		"AuthorEmail":   settings["author_email"],
		"LastBuildDate": lastBuildDate,
		"FullContent":   true,
	}

	tmpl, err := template.New("rss").Parse(rssTemplate)
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "RSS 模板解析失败"})
	}

	var buf bytes.Buffer
	if err := tmpl.Execute(&buf, data); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "RSS 生成失败"})
	}

	c.Set("Content-Type", "application/xml; charset=utf-8")
	return c.Send(buf.Bytes())
}

func (h *PostHandler) Sitemap(c *fiber.Ctx) error {
	settings, err := h.settingService.GetPublic()
	if err != nil {
		settings = make(map[string]string)
	}

	siteURL := settings["site_url"]
	if siteURL == "" {
		siteURL = c.Protocol() + "://" + c.Hostname()
	}

	posts, _, err := h.postService.List(services.PostListParams{
		Page:     1,
		PageSize: 1000,
		Status:   models.PostStatusPublished,
	})
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "获取文章失败"})
	}

	pages, _, err := h.pageService.List(services.PageListParams{
		Page:     1,
		PageSize: 500,
		Status:   models.PageStatusPublished,
	})

	categories, _ := h.categoryService.List()
	tags, _ := h.tagService.List()

	lastMod := time.Now().Format("2006-01-02")
	if len(posts) > 0 {
		if posts[0].PublishedAt != nil {
			lastMod = posts[0].PublishedAt.Format("2006-01-02")
		} else {
			lastMod = posts[0].CreatedAt.Format("2006-01-02")
		}
	}

	data := map[string]interface{}{
		"Posts":       posts,
		"Pages":       pages,
		"Categories":  categories,
		"Tags":        tags,
		"SiteURL":     siteURL,
		"LastMod":     lastMod,
		"Changefreq":  "weekly",
		"Priority":    "0.8",
	}

	tmpl, err := template.New("sitemap").Parse(sitemapTemplate)
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "Sitemap 模板解析失败"})
	}

	var buf bytes.Buffer
	if err := tmpl.Execute(&buf, data); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "Sitemap 生成失败"})
	}

	c.Set("Content-Type", "application/xml; charset=utf-8")
	return c.Send(buf.Bytes())
}
