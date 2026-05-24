package theme

import (
	"bytes"
	"context"

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

func renderMarkdown(content string) string {
	var buf bytes.Buffer
	if err := md.Convert([]byte(content), &buf); err != nil {
		return content
	}
	return buf.String()
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
