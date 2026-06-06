package theme

import (
	"bytes"
	"context"
	"sync"

	chromahtml "github.com/alecthomas/chroma/v2/formatters/html"
	"github.com/yuin/goldmark"
	highlighting "github.com/yuin/goldmark-highlighting/v2"
	"github.com/yuin/goldmark/extension"
	goldmarkhtml "github.com/yuin/goldmark/renderer/html"
)

var md = goldmark.New(
	goldmark.WithExtensions(
		extension.GFM,
		extension.NewFootnote(),
		highlighting.NewHighlighting(
			highlighting.WithStyle("github-dark"),
			highlighting.WithFormatOptions(chromahtml.WithLineNumbers(true)),
		),
	),
	goldmark.WithRendererOptions(
		goldmarkhtml.WithUnsafe(),
		goldmarkhtml.WithHardWraps(),
		goldmarkhtml.WithXHTML(),
	),
)

var globalFilterApplier FilterApplier

type FilterApplier interface {
	ApplyFilter(ctx context.Context, name string, data interface{}) (interface{}, error)
}

func SetFilterApplier(fa FilterApplier) {
	globalFilterApplier = fa
}

const markdownCacheSize = 256

type markdownCache struct {
	items map[string]string
	keys  []string
	mu    sync.RWMutex
}

var mdCache = &markdownCache{
	items: make(map[string]string, markdownCacheSize),
	keys:  make([]string, 0, markdownCacheSize),
}

func (c *markdownCache) Get(key string) (string, bool) {
	c.mu.RLock()
	defer c.mu.RUnlock()
	v, ok := c.items[key]
	return v, ok
}

func (c *markdownCache) Set(key string, value string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if _, exists := c.items[key]; exists {
		c.items[key] = value
		return
	}
	if len(c.keys) >= markdownCacheSize {
		evictKey := c.keys[0]
		delete(c.items, evictKey)
		c.keys = c.keys[1:]
	}
	c.items[key] = value
	c.keys = append(c.keys, key)
}

func renderMarkdown(content string) string {
	if cached, ok := mdCache.Get(content); ok {
		return cached
	}

	var buf bytes.Buffer
	if err := md.Convert([]byte(content), &buf); err != nil {
		return content
	}
	result := buf.String()
	mdCache.Set(content, result)
	return result
}

func renderMarkdownWithFilter(content string) string {
	htmlContent := renderMarkdown(content)

	if globalFilterApplier != nil {
		if filtered, err := globalFilterApplier.ApplyFilter(context.Background(), "post.content", htmlContent); err == nil {
			if s, ok := filtered.(string); ok {
				return s
			}
		}
	}

	return htmlContent
}
