package web

import (
	"bytes"
	"compress/gzip"
	"encoding/json"
	"fmt"
	"net/http"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
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

	siteCache   siteInfoCache
	siteCacheMu sync.RWMutex

	cache    sync.Map
	sfGroup  sync.Map
	cacheLen int32

	commonHeaders [][2]string
}

type htmlCacheEntry struct {
	raw        []byte
	gz         []byte
	statusCode int
	expiresAt  int64
	gzHeaders  [][2]string
	rawHeaders [][2]string
}

type siteInfoCache struct {
	info         SiteInfo
	lang         string
	navItems     []NavItem
	postCount    int64
	commentCount int64
	expiresAt    time.Time
}

const (
	siteCacheTTL    = 5 * time.Minute
	htmlCacheTTL    = 2 * time.Minute
	htmlCacheMaxKey = 256
)

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
		commonHeaders: [][2]string{
			{"X-Content-Type-Options", "nosniff"},
			{"X-Frame-Options", "DENY"},
			{"X-XSS-Protection", "1; mode=block"},
			{"Referrer-Policy", "strict-origin-when-cross-origin"},
			{"Content-Security-Policy", "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; font-src 'self' https:; connect-src 'self' https:; frame-src https:;"},
		},
	}
}

func (h *PageHandler) getHTMLCache(key string) (*htmlCacheEntry, bool) {
	val, ok := h.cache.Load(key)
	if !ok {
		return nil, false
	}
	entry := val.(*htmlCacheEntry)
	if time.Now().UnixNano() > entry.expiresAt {
		return nil, false
	}
	return entry, true
}

func (h *PageHandler) setHTMLCache(key string, raw []byte, contentType string, statusCode int) {
	gz := gzipBytes(raw)
	rawLen := strconv.Itoa(len(raw))
	gzLen := strconv.Itoa(len(gz))

	baseHeaders := make([][2]string, 0, len(h.commonHeaders)+4)
	baseHeaders = append(baseHeaders, h.commonHeaders...)
	baseHeaders = append(baseHeaders,
		[2]string{"Content-Type", contentType},
		[2]string{"X-Cache", "HIT"},
	)

	rawHeaders := make([][2]string, len(baseHeaders)+1)
	copy(rawHeaders, baseHeaders)
	rawHeaders[len(rawHeaders)-1] = [2]string{"Content-Length", rawLen}

	gzHeaders := make([][2]string, len(baseHeaders)+2)
	copy(gzHeaders, baseHeaders)
	gzHeaders[len(baseHeaders)] = [2]string{"Content-Encoding", "gzip"}
	gzHeaders[len(baseHeaders)+1] = [2]string{"Content-Length", gzLen}

	if atomic.LoadInt32(&h.cacheLen) > htmlCacheMaxKey {
		now := time.Now().UnixNano()
		h.cache.Range(func(k, v interface{}) bool {
			e := v.(*htmlCacheEntry)
			if now > e.expiresAt {
				h.cache.Delete(k)
				atomic.AddInt32(&h.cacheLen, -1)
			}
			return true
		})
	}

	h.cache.Store(key, &htmlCacheEntry{
		raw:        raw,
		gz:         gz,
		statusCode: statusCode,
		expiresAt:  time.Now().Add(htmlCacheTTL).UnixNano(),
		gzHeaders:  gzHeaders,
		rawHeaders: rawHeaders,
	})
	atomic.StoreInt32(&h.cacheLen, 0)
	count := int32(0)
	h.cache.Range(func(_, _ interface{}) bool {
		count++
		return true
	})
	atomic.StoreInt32(&h.cacheLen, count)
}

func gzipBytes(data []byte) []byte {
	var buf bytes.Buffer
	buf.Grow(len(data) / 2)
	gw, _ := gzip.NewWriterLevel(&buf, gzip.BestSpeed)
	gw.Write(data)
	gw.Close()
	return buf.Bytes()
}

func (h *PageHandler) InvalidateHTMLCache() {
	h.cache.Range(func(k, _ interface{}) bool {
		h.cache.Delete(k)
		return true
	})
	atomic.StoreInt32(&h.cacheLen, 0)
}

func (h *PageHandler) TryServeCached(w http.ResponseWriter, r *http.Request) bool {
	cacheKey := h.resolveCacheKey(r)
	if cacheKey == "" {
		return false
	}

	entry, ok := h.getHTMLCache(cacheKey)
	if !ok {
		return false
	}

	acceptGzip := strings.Contains(r.Header.Get("Accept-Encoding"), "gzip")
	hdr := w.Header()

	if acceptGzip && len(entry.gz) > 0 {
		for _, kv := range entry.gzHeaders {
			hdr.Set(kv[0], kv[1])
		}
		w.WriteHeader(entry.statusCode)
		w.Write(entry.gz)
	} else {
		for _, kv := range entry.rawHeaders {
			hdr.Set(kv[0], kv[1])
		}
		w.WriteHeader(entry.statusCode)
		w.Write(entry.raw)
	}
	return true
}

func (h *PageHandler) resolveCacheKey(r *http.Request) string {
	path := r.URL.Path
	if r.Method != http.MethodGet {
		return ""
	}

	switch {
	case path == "/":
		page := r.URL.Query().Get("page")
		if page == "" || page == "1" {
			return "index:1"
		}
		return "index:" + page
	case len(path) > 7 && path[:7] == "/posts/":
		return "post:" + path[7:]
	case path == "/archives":
		return "archives"
	case path == "/categories":
		return "categories"
	case len(path) > 12 && path[:12] == "/categories/":
		page := r.URL.Query().Get("page")
		if page == "" || page == "1" {
			return "category:" + path[12:] + ":1"
		}
		return "category:" + path[12:] + ":" + page
	case path == "/tags":
		return "tags"
	case len(path) > 6 && path[:6] == "/tags/":
		page := r.URL.Query().Get("page")
		if page == "" || page == "1" {
			return "tag:" + path[6:] + ":1"
		}
		return "tag:" + path[6:] + ":" + page
	case len(path) > 3 && path[:3] == "/p/":
		return "page:" + path[3:]
	}
	return ""
}

type cacheableResponse struct {
	buf         bytes.Buffer
	statusCode  int
	wroteHeader bool
}

func (cr *cacheableResponse) Header() http.Header {
	return http.Header{}
}

func (cr *cacheableResponse) Write(b []byte) (int, error) {
	return cr.buf.Write(b)
}

func (cr *cacheableResponse) WriteHeader(code int) {
	if !cr.wroteHeader {
		cr.statusCode = code
		cr.wroteHeader = true
	}
}

type sfResult struct {
	html        []byte
	contentType string
	statusCode  int
}

func (h *PageHandler) renderWithCache(w http.ResponseWriter, r *http.Request, cacheKey string, renderFn func(w http.ResponseWriter, r *http.Request)) {
	if entry, ok := h.getHTMLCache(cacheKey); ok {
		acceptGzip := strings.Contains(r.Header.Get("Accept-Encoding"), "gzip")
		hdr := w.Header()
		if acceptGzip && len(entry.gz) > 0 {
			for _, kv := range entry.gzHeaders {
				hdr.Set(kv[0], kv[1])
			}
			w.WriteHeader(entry.statusCode)
			w.Write(entry.gz)
		} else {
			for _, kv := range entry.rawHeaders {
				hdr.Set(kv[0], kv[1])
			}
			w.WriteHeader(entry.statusCode)
			w.Write(entry.raw)
		}
		return
	}

	val, _ := h.sfGroup.LoadOrStore(cacheKey, &sync.Once{})
	once := val.(*sync.Once)

	var result *sfResult
	once.Do(func() {
		cr := &cacheableResponse{statusCode: http.StatusOK}
		renderFn(cr, r)

		html := cr.buf.Bytes()
		contentType := "text/html; charset=utf-8"
		h.setHTMLCache(cacheKey, html, contentType, cr.statusCode)
		result = &sfResult{html: html, contentType: contentType, statusCode: cr.statusCode}
	})

	h.sfGroup.Delete(cacheKey)

	if result == nil {
		if entry, ok := h.getHTMLCache(cacheKey); ok {
			acceptGzip := strings.Contains(r.Header.Get("Accept-Encoding"), "gzip")
			hdr := w.Header()
			if acceptGzip && len(entry.gz) > 0 {
				for _, kv := range entry.gzHeaders {
					hdr.Set(kv[0], kv[1])
				}
				w.WriteHeader(entry.statusCode)
				w.Write(entry.gz)
			} else {
				for _, kv := range entry.rawHeaders {
					hdr.Set(kv[0], kv[1])
				}
				w.WriteHeader(entry.statusCode)
				w.Write(entry.raw)
			}
			return
		}
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}

	acceptGzip := strings.Contains(r.Header.Get("Accept-Encoding"), "gzip")
	w.Header().Set("Content-Type", result.contentType)
	w.Header().Set("X-Cache", "MISS")
	if acceptGzip {
		if entry, ok := h.getHTMLCache(cacheKey); ok && len(entry.gz) > 0 {
			w.Header().Set("Content-Encoding", "gzip")
			w.Header().Set("Content-Length", fmt.Sprintf("%d", len(entry.gz)))
			w.WriteHeader(entry.statusCode)
			w.Write(entry.gz)
		} else {
			w.Header().Set("Content-Length", strconv.Itoa(len(result.html)))
			w.WriteHeader(result.statusCode)
			w.Write(result.html)
		}
	} else {
		w.Header().Set("Content-Length", strconv.Itoa(len(result.html)))
		w.WriteHeader(result.statusCode)
		w.Write(result.html)
	}
}

func (h *PageHandler) Index(w http.ResponseWriter, r *http.Request) {
	page, _ := strconv.Atoi(r.URL.Query().Get("page"))
	if page <= 0 {
		page = 1
	}

	cacheKey := "index:" + strconv.Itoa(page)
	h.renderWithCache(w, r, cacheKey, func(w http.ResponseWriter, r *http.Request) {
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
	})
}

func (h *PageHandler) Post(w http.ResponseWriter, r *http.Request) {
	slug := chi.URLParam(r, "slug")

	cacheKey := "post:" + slug
	h.renderWithCache(w, r, cacheKey, func(w http.ResponseWriter, r *http.Request) {
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
	})
}

func (h *PageHandler) Archives(w http.ResponseWriter, r *http.Request) {
	cacheKey := "archives"
	h.renderWithCache(w, r, cacheKey, func(w http.ResponseWriter, r *http.Request) {
		posts, _, _ := h.postService.ListByType(r.Context(), "post", 1, 200, "published")
		ctx := h.baseContext(r)
		ctx["posts"] = posts

		if err := h.themeManager.Renderer().Render(w, "archive.html", ctx); err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
		}
	})
}

func (h *PageHandler) CustomPage(w http.ResponseWriter, r *http.Request) {
	slug := chi.URLParam(r, "slug")

	cacheKey := "page:" + slug
	h.renderWithCache(w, r, cacheKey, func(w http.ResponseWriter, r *http.Request) {
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
	})
}

func (h *PageHandler) NotFound(w http.ResponseWriter, r *http.Request) {
	w.WriteHeader(http.StatusNotFound)
	ctx := h.baseContext(r)

	if err := h.themeManager.Renderer().Render(w, "404.html", ctx); err != nil {
		w.Write([]byte("404 Not Found"))
	}
}

func (h *PageHandler) baseContext(r *http.Request) pongo2.Context {
	lang := "en"
	var siteInfo SiteInfo
	var navItems []NavItem
	var postCount, commentCount int64

	now := time.Now()
	h.siteCacheMu.RLock()
	if now.Before(h.siteCache.expiresAt) {
		siteInfo = h.siteCache.info
		lang = h.siteCache.lang
		navItems = h.siteCache.navItems
		postCount = h.siteCache.postCount
		commentCount = h.siteCache.commentCount
		h.siteCacheMu.RUnlock()
	} else {
		h.siteCacheMu.RUnlock()
		siteInfo = h.siteInfo
		if h.options != nil {
			opts, _ := h.options.GetMulti(r.Context(), []string{
				"site_title", "site_description", "site_url",
				"site_icon", "site_language", "nav_items",
			})
			if v, ok := opts["site_title"]; ok && v != "" {
				siteInfo.Title = v
			}
			if v, ok := opts["site_description"]; ok && v != "" {
				siteInfo.Description = v
			}
			if v, ok := opts["site_url"]; ok && v != "" {
				siteInfo.URL = v
			}
			if v, ok := opts["site_icon"]; ok && v != "" {
				siteInfo.Icon = v
			}
			if v, ok := opts["site_language"]; ok && v != "" {
				lang = v
			}
			if v, ok := opts["nav_items"]; ok && v != "" {
				json.Unmarshal([]byte(v), &navItems)
			}
		}
		if h.postService != nil {
			if count, err := h.postService.CountByType(r.Context(), "post", "published"); err == nil {
				postCount = count
			}
		}
		if h.commentService != nil {
			if count, err := h.commentService.Count(r.Context(), "approved"); err == nil {
				commentCount = count
			}
		}
		newCache := siteInfoCache{
			info:         siteInfo,
			lang:         lang,
			navItems:     navItems,
			postCount:    postCount,
			commentCount: commentCount,
			expiresAt:    now.Add(siteCacheTTL),
		}
		h.siteCacheMu.Lock()
		h.siteCache = newCache
		h.siteCacheMu.Unlock()
	}

	if cookie, err := r.Cookie("polaris_lang"); err == nil && cookie.Value != "" {
		lang = cookie.Value
	}

	ctx := pongo2.Context{
		"site":          siteInfo,
		"current_year":  now.Year(),
		"request":       r,
		"lang":          lang,
		"post_count":    postCount,
		"comment_count": commentCount,
	}
	if len(navItems) > 0 {
		ctx["nav_items"] = navItems
	}
	if h.i18nBundle != nil {
		ctx["t"] = func(key string) string {
			return h.i18nBundle.T(lang, key)
		}
	} else {
		ctx["t"] = func(key string) string { return key }
	}
	if h.themeManager != nil {
		if active := h.themeManager.GetActiveTheme(); active != nil {
			ctx["theme_settings"] = active.Settings
		}
	}
	return ctx
}

func (h *PageHandler) Categories(w http.ResponseWriter, r *http.Request) {
	cacheKey := "categories"
	h.renderWithCache(w, r, cacheKey, func(w http.ResponseWriter, r *http.Request) {
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
	})
}

func (h *PageHandler) CategoryDetail(w http.ResponseWriter, r *http.Request) {
	slug := chi.URLParam(r, "slug")
	page, _ := strconv.Atoi(r.URL.Query().Get("page"))
	if page <= 0 {
		page = 1
	}

	cacheKey := "category:" + slug + ":" + strconv.Itoa(page)
	h.renderWithCache(w, r, cacheKey, func(w http.ResponseWriter, r *http.Request) {
		category, err := h.categoryService.GetBySlug(r.Context(), slug)
		if err != nil {
			h.NotFound(w, r)
			return
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
	})
}

func (h *PageHandler) Tags(w http.ResponseWriter, r *http.Request) {
	cacheKey := "tags"
	h.renderWithCache(w, r, cacheKey, func(w http.ResponseWriter, r *http.Request) {
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
	})
}

func (h *PageHandler) TagDetail(w http.ResponseWriter, r *http.Request) {
	slug := chi.URLParam(r, "slug")
	page, _ := strconv.Atoi(r.URL.Query().Get("page"))
	if page <= 0 {
		page = 1
	}

	cacheKey := "tag:" + slug + ":" + strconv.Itoa(page)
	h.renderWithCache(w, r, cacheKey, func(w http.ResponseWriter, r *http.Request) {
		tag, err := h.tagService.GetBySlug(r.Context(), slug)
		if err != nil {
			h.NotFound(w, r)
			return
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
	})
}
