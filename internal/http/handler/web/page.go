package web

import (
	"encoding/json"
	"net/http"
	"strconv"
	"time"

	"github.com/flosch/pongo2/v6"
	"github.com/go-chi/chi/v5"
	"github.com/polaris-blog/blog/internal/i18n"
	"github.com/polaris-blog/blog/internal/repository"
	"github.com/polaris-blog/blog/internal/service"
	"github.com/polaris-blog/blog/internal/theme"
)

type NavItem struct {
	Label string `json:"label"`
	URL   string `json:"url"`
}

type PageHandler struct {
	themeManager    *theme.Manager
	postService     *service.PostService
	commentService  *service.CommentService
	categoryService *service.CategoryService
	tagService      *service.TagService
	options         repository.OptionRepository
	siteInfo        SiteInfo
	i18nBundle      *i18n.Bundle
}

type SiteInfo struct {
	Title       string
	Description string
	URL         string
	Icon        string
}

type Pager struct {
	CurrentPage int
	TotalPages  int
	HasPrev     bool
	HasNext     bool
	PrevPage    int
	NextPage    int
}

func NewPageHandler(themeManager *theme.Manager, postService *service.PostService, commentService *service.CommentService, categoryService *service.CategoryService, tagService *service.TagService, options repository.OptionRepository, siteInfo SiteInfo, i18nBundle *i18n.Bundle) *PageHandler {
	return &PageHandler{
		themeManager:    themeManager,
		postService:     postService,
		commentService:  commentService,
		categoryService: categoryService,
		tagService:      tagService,
		options:         options,
		siteInfo:        siteInfo,
		i18nBundle:      i18nBundle,
	}
}

func (h *PageHandler) Index(w http.ResponseWriter, r *http.Request) {
	page, _ := strconv.Atoi(r.URL.Query().Get("page"))
	if page <= 0 {
		page = 1
	}

	posts, total, err := h.postService.ListByType(r.Context(), "post", page, 10, "published")
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	totalPages := int(total) / 10
	if int(total)%10 > 0 {
		totalPages++
	}
	if totalPages < 1 {
		totalPages = 1
	}

	ctx := h.baseContext(r)
	ctx["posts"] = posts
	ctx["pager"] = Pager{
		CurrentPage: page,
		TotalPages:  totalPages,
		HasPrev:     page > 1,
		HasNext:     page < totalPages,
		PrevPage:    page - 1,
		NextPage:    page + 1,
	}

	if err := h.themeManager.Renderer().Render(w, "index.html", ctx); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
	}
}

func (h *PageHandler) Post(w http.ResponseWriter, r *http.Request) {
	slug := chi.URLParam(r, "slug")
	post, err := h.postService.GetBySlug(r.Context(), slug)
	if err != nil {
		h.NotFound(w, r)
		return
	}

	if post.Type == "page" {
		http.Redirect(w, r, "/p/"+post.Slug, http.StatusMovedPermanently)
		return
	}

	ctx := h.baseContext(r)
	ctx["post"] = post

	if h.commentService != nil {
		comments, _ := h.commentService.GetByPost(r.Context(), post.ID)
		tree := h.commentService.BuildTree(comments)
		ctx["flat_comments"] = h.commentService.FlattenTree(tree)
	}

	if err := h.themeManager.Renderer().Render(w, "post.html", ctx); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
	}
}

func (h *PageHandler) Archives(w http.ResponseWriter, r *http.Request) {
	posts, _, _ := h.postService.ListByType(r.Context(), "post", 1, 1000, "published")
	ctx := h.baseContext(r)
	ctx["posts"] = posts

	if err := h.themeManager.Renderer().Render(w, "archive.html", ctx); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
	}
}

func (h *PageHandler) CustomPage(w http.ResponseWriter, r *http.Request) {
	slug := chi.URLParam(r, "slug")
	page, err := h.postService.GetBySlug(r.Context(), slug)
	if err != nil || page.Type != "page" {
		h.NotFound(w, r)
		return
	}

	ctx := h.baseContext(r)
	ctx["page"] = page

	if err := h.themeManager.Renderer().Render(w, "page.html", ctx); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
	}
}

func (h *PageHandler) NotFound(w http.ResponseWriter, r *http.Request) {
	w.WriteHeader(http.StatusNotFound)
	ctx := h.baseContext(r)

	if err := h.themeManager.Renderer().Render(w, "404.html", ctx); err != nil {
		w.Write([]byte("404 Not Found"))
	}
}

func (h *PageHandler) baseContext(r *http.Request) pongo2.Context {
	siteInfo := h.siteInfo
	lang := "en"
	if h.options != nil {
		if title, err := h.options.Get(r.Context(), "site_title"); err == nil && title != "" {
			siteInfo.Title = title
		}
		if desc, err := h.options.Get(r.Context(), "site_description"); err == nil && desc != "" {
			siteInfo.Description = desc
		}
		if url, err := h.options.Get(r.Context(), "site_url"); err == nil && url != "" {
			siteInfo.URL = url
		}
		if icon, err := h.options.Get(r.Context(), "site_icon"); err == nil && icon != "" {
			siteInfo.Icon = icon
		}
		if siteLang, err := h.options.Get(r.Context(), "site_language"); err == nil && siteLang != "" {
			lang = siteLang
		}
	}
	if cookie, err := r.Cookie("polaris_lang"); err == nil && cookie.Value != "" {
		lang = cookie.Value
	}
	ctx := pongo2.Context{
		"site":         siteInfo,
		"current_year": time.Now().Year(),
		"request":      r,
		"lang":         lang,
	}
	if h.i18nBundle != nil {
		ctx["t"] = func(key string) string {
			return h.i18nBundle.T(lang, key)
		}
	} else {
		ctx["t"] = func(key string) string { return key }
	}
	if h.options != nil {
		if navJSON, err := h.options.Get(r.Context(), "nav_items"); err == nil && navJSON != "" {
			var navItems []NavItem
			if json.Unmarshal([]byte(navJSON), &navItems) == nil {
				ctx["nav_items"] = navItems
			}
		}
	}
	if h.themeManager != nil {
		if active := h.themeManager.GetActiveTheme(); active != nil {
			ctx["theme_settings"] = active.Settings
		}
	}
	if h.postService != nil {
		if count, err := h.postService.CountByType(r.Context(), "post", "published"); err == nil {
			ctx["post_count"] = count
		}
	}
	if h.commentService != nil {
		if count, err := h.commentService.Count(r.Context(), "approved"); err == nil {
			ctx["comment_count"] = count
		}
	}
	return ctx
}

func (h *PageHandler) Categories(w http.ResponseWriter, r *http.Request) {
	categories, err := h.categoryService.List(r.Context())
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	ctx := h.baseContext(r)
	ctx["categories"] = categories

	if err := h.themeManager.Renderer().Render(w, "categories.html", ctx); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
	}
}

func (h *PageHandler) CategoryDetail(w http.ResponseWriter, r *http.Request) {
	slug := chi.URLParam(r, "slug")
	category, err := h.categoryService.GetBySlug(r.Context(), slug)
	if err != nil {
		h.NotFound(w, r)
		return
	}

	page, _ := strconv.Atoi(r.URL.Query().Get("page"))
	if page <= 0 {
		page = 1
	}

	posts, total, err := h.postService.ListByCategory(r.Context(), category.ID, page, 10)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	totalPages := int(total) / 10
	if int(total)%10 > 0 {
		totalPages++
	}
	if totalPages < 1 {
		totalPages = 1
	}

	ctx := h.baseContext(r)
	ctx["category"] = category
	ctx["posts"] = posts
	ctx["pager"] = Pager{
		CurrentPage: page,
		TotalPages:  totalPages,
		HasPrev:     page > 1,
		HasNext:     page < totalPages,
		PrevPage:    page - 1,
		NextPage:    page + 1,
	}

	if err := h.themeManager.Renderer().Render(w, "category_detail.html", ctx); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
	}
}

func (h *PageHandler) Tags(w http.ResponseWriter, r *http.Request) {
	tags, err := h.tagService.List(r.Context())
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	ctx := h.baseContext(r)
	ctx["tags"] = tags

	if err := h.themeManager.Renderer().Render(w, "tags.html", ctx); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
	}
}

func (h *PageHandler) TagDetail(w http.ResponseWriter, r *http.Request) {
	slug := chi.URLParam(r, "slug")
	tag, err := h.tagService.GetBySlug(r.Context(), slug)
	if err != nil {
		h.NotFound(w, r)
		return
	}

	page, _ := strconv.Atoi(r.URL.Query().Get("page"))
	if page <= 0 {
		page = 1
	}

	posts, total, err := h.postService.ListByTag(r.Context(), tag.ID, page, 10)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	totalPages := int(total) / 10
	if int(total)%10 > 0 {
		totalPages++
	}
	if totalPages < 1 {
		totalPages = 1
	}

	ctx := h.baseContext(r)
	ctx["tag"] = tag
	ctx["posts"] = posts
	ctx["pager"] = Pager{
		CurrentPage: page,
		TotalPages:  totalPages,
		HasPrev:     page > 1,
		HasNext:     page < totalPages,
		PrevPage:    page - 1,
		NextPage:    page + 1,
	}

	if err := h.themeManager.Renderer().Render(w, "tag_detail.html", ctx); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
	}
}
