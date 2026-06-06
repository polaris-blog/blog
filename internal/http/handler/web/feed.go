package web

import (
	"fmt"
	"net/http"
	"strings"
	"time"
)

func (h *PageHandler) RSS(w http.ResponseWriter, r *http.Request) {
	siteInfo := h.getSiteInfo(r)

	posts, _, err := h.postService.ListByType(r.Context(), "post", 1, 20, "published")
	if err != nil {
		http.Error(w, "failed to load posts", http.StatusInternalServerError)
		return
	}

	baseURL := strings.TrimRight(siteInfo.URL, "/")

	var items strings.Builder
	for _, p := range posts {
		pubDate := ""
		if p.PublishedAt != nil {
			pubDate = p.PublishedAt.UTC().Format(time.RFC1123)
		}
		link := fmt.Sprintf("%s/posts/%s", baseURL, p.Slug)
		desc := p.Excerpt
		if desc == "" && len(p.Content) > 200 {
			desc = p.Content[:200] + "..."
		}
		desc = escapeXML(desc)

		items.WriteString(fmt.Sprintf(
			`    <item>
      <title>%s</title>
      <link>%s</link>
      <description>%s</description>
      <pubDate>%s</pubDate>
      <guid>%s</guid>
    </item>
`,
			escapeXML(p.Title), link, desc, pubDate, link,
		))
	}

	feed := fmt.Sprintf(`<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom">
  <channel>
    <title>%s</title>
    <link>%s</link>
    <description>%s</description>
    <language>zh-cn</language>
    <lastBuildDate>%s</lastBuildDate>
    <atom:link href="%s/feed.xml" rel="self" type="application/rss+xml"/>
%s  </channel>
</rss>`,
		escapeXML(siteInfo.Title),
		baseURL,
		escapeXML(siteInfo.Description),
		time.Now().UTC().Format(time.RFC1123),
		baseURL,
		items.String(),
	)

	w.Header().Set("Content-Type", "application/xml; charset=utf-8")
	w.Header().Set("Cache-Control", "public, max-age=3600")
	w.Write([]byte(feed))
}

func (h *PageHandler) Sitemap(w http.ResponseWriter, r *http.Request) {
	siteInfo := h.getSiteInfo(r)

	posts, err := h.postService.ListSlugs(r.Context(), "post", "published", 500)
	if err != nil {
		http.Error(w, "failed to load posts", http.StatusInternalServerError)
		return
	}

	categories, _ := h.categoryService.List(r.Context())
	tags, _ := h.tagService.List(r.Context())

	baseURL := strings.TrimRight(siteInfo.URL, "/")

	var urls strings.Builder

	urls.WriteString(sitemapURL(baseURL, time.Now(), "1.0", "daily"))

	urls.WriteString(sitemapURL(baseURL+"/archives", time.Now(), "0.8", "weekly"))

	for _, c := range categories {
		urls.WriteString(sitemapURL(fmt.Sprintf("%s/categories/%s", baseURL, c.Slug), c.UpdatedAt, "0.6", "weekly"))
	}

	for _, t := range tags {
		urls.WriteString(sitemapURL(fmt.Sprintf("%s/tags/%s", baseURL, t.Slug), t.UpdatedAt, "0.6", "weekly"))
	}

	for _, p := range posts {
		modTime := p.UpdatedAt
		if p.PublishedAt != nil && p.PublishedAt.After(modTime) {
			modTime = *p.PublishedAt
		}
		urls.WriteString(sitemapURL(fmt.Sprintf("%s/posts/%s", baseURL, p.Slug), modTime, "0.8", "monthly"))
	}

	sitemap := fmt.Sprintf(`<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
%s</urlset>`, urls.String())

	w.Header().Set("Content-Type", "application/xml; charset=utf-8")
	w.Header().Set("Cache-Control", "public, max-age=3600")
	w.Write([]byte(sitemap))
}

func sitemapURL(loc string, lastMod time.Time, priority, changefreq string) string {
	return fmt.Sprintf(
		"  <url>\n    <loc>%s</loc>\n    <lastmod>%s</lastmod>\n    <priority>%s</priority>\n    <changefreq>%s</changefreq>\n  </url>\n",
		escapeXML(loc),
		lastMod.UTC().Format("2006-01-02T15:04:05Z07:00"),
		priority,
		changefreq,
	)
}

func escapeXML(s string) string {
	s = strings.ReplaceAll(s, "&", "&amp;")
	s = strings.ReplaceAll(s, "<", "&lt;")
	s = strings.ReplaceAll(s, ">", "&gt;")
	s = strings.ReplaceAll(s, "'", "&apos;")
	s = strings.ReplaceAll(s, "\"", "&quot;")
	return s
}

func (h *PageHandler) getSiteInfo(r *http.Request) SiteInfo {
	siteInfo := h.siteInfo
	if h.options != nil {
		opts, _ := h.options.GetMulti(r.Context(), []string{"site_title", "site_description", "site_url"})
		if v, ok := opts["site_title"]; ok && v != "" {
			siteInfo.Title = v
		}
		if v, ok := opts["site_description"]; ok && v != "" {
			siteInfo.Description = v
		}
		if v, ok := opts["site_url"]; ok && v != "" {
			siteInfo.URL = v
		}
	}
	return siteInfo
}
