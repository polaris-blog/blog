package theme

import (
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sync"

	"github.com/flosch/pongo2/v6"
)

type Renderer struct {
	loader     *Loader
	customFunc map[string]interface{}
	mu         sync.RWMutex
	debug      bool
}

func NewRenderer(loader *Loader, debug bool) *Renderer {
	r := &Renderer{
		loader:     loader,
		customFunc: make(map[string]interface{}),
		debug:      debug,
	}
	r.registerBuiltinFunctions()
	return r
}

func (r *Renderer) Render(w io.Writer, name string, ctx pongo2.Context) error {
	r.mu.RLock()
	defer r.mu.RUnlock()

	tpl, err := r.getTemplate(name)
	if err != nil {
		return fmt.Errorf("get template %s: %w", name, err)
	}

	return tpl.ExecuteWriter(ctx, w)
}

func (r *Renderer) RenderString(tplStr string, ctx pongo2.Context) (string, error) {
	tpl, err := pongo2.FromString(tplStr)
	if err != nil {
		return "", err
	}
	return tpl.Execute(ctx)
}

func (r *Renderer) RegisterFunction(name string, fn interface{}) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.customFunc[name] = fn
}

func (r *Renderer) ReloadTemplates() {
	if r.loader != nil {
		r.loader.ClearCache()
	}
}

func (r *Renderer) getTemplate(name string) (*pongo2.Template, error) {
	if r.debug {
		r.loader.ClearCache()
	}
	return r.loader.Get(name)
}

func (r *Renderer) registerBuiltinFunctions() {
	pongo2.RegisterFilter("truncate", filterTruncate)
	pongo2.RegisterFilter("date_format", filterDateFormat)
	pongo2.RegisterFilter("markdown", filterMarkdown)
	pongo2.RegisterFilter("strip_tags", filterStripTags)
	pongo2.RegisterFilter("word_count", filterWordCount)
	pongo2.RegisterFilter("add", filterAdd)
	pongo2.RegisterFilter("mul", filterMul)
	pongo2.RegisterFilter("parse_browser", filterParseBrowser)
}

func filterTruncate(in *pongo2.Value, param *pongo2.Value) (*pongo2.Value, *pongo2.Error) {
	length := param.Integer()
	s := in.String()
	if len(s) <= length {
		return in, nil
	}
	return pongo2.AsValue(s[:length] + "..."), nil
}

func filterDateFormat(in *pongo2.Value, param *pongo2.Value) (*pongo2.Value, *pongo2.Error) {
	format := param.String()
	if format == "" {
		format = "2006-01-02 15:04:05"
	}
	t := in.Time()
	if t.IsZero() {
		return pongo2.AsValue(""), nil
	}
	return pongo2.AsValue(t.Format(format)), nil
}

func filterMarkdown(in *pongo2.Value, param *pongo2.Value) (*pongo2.Value, *pongo2.Error) {
	return pongo2.AsSafeValue(renderMarkdownWithFilter(in.String())), nil
}

func filterStripTags(in *pongo2.Value, param *pongo2.Value) (*pongo2.Value, *pongo2.Error) {
	s := in.String()
	var result []byte
	inTag := false
	for _, c := range s {
		if c == '<' {
			inTag = true
			continue
		}
		if c == '>' {
			inTag = false
			continue
		}
		if !inTag {
			result = append(result, string(c)...)
		}
	}
	return pongo2.AsValue(string(result)), nil
}

func filterWordCount(in *pongo2.Value, param *pongo2.Value) (*pongo2.Value, *pongo2.Error) {
	s := in.String()
	count := 0
	inWord := false
	for _, c := range s {
		if c == ' ' || c == '\n' || c == '\t' || c == '\r' {
			if inWord {
				count++
				inWord = false
			}
		} else {
			inWord = true
		}
	}
	if inWord {
		count++
	}
	return pongo2.AsValue(count), nil
}

func filterAdd(in *pongo2.Value, param *pongo2.Value) (*pongo2.Value, *pongo2.Error) {
	return pongo2.AsValue(in.Integer() + param.Integer()), nil
}

func filterMul(in *pongo2.Value, param *pongo2.Value) (*pongo2.Value, *pongo2.Error) {
	return pongo2.AsValue(in.Integer() * param.Integer()), nil
}

func filterParseBrowser(in *pongo2.Value, param *pongo2.Value) (*pongo2.Value, *pongo2.Error) {
	ua := in.String()
	browser := parseBrowserInfo(ua)
	return pongo2.AsValue(browser), nil
}

type Loader struct {
	dirs  []string
	cache map[string]*pongo2.Template
	mu    sync.RWMutex
}

func NewLoader(dirs ...string) *Loader {
	return &Loader{
		dirs:  dirs,
		cache: make(map[string]*pongo2.Template),
	}
}

func (l *Loader) Get(name string) (*pongo2.Template, error) {
	l.mu.RLock()
	if tpl, ok := l.cache[name]; ok {
		l.mu.RUnlock()
		return tpl, nil
	}
	l.mu.RUnlock()

	for _, dir := range l.dirs {
		path := filepath.Join(dir, name)
		if _, err := os.Stat(path); err == nil {
			tpl, err := pongo2.FromFile(path)
			if err != nil {
				return nil, err
			}

			l.mu.Lock()
			l.cache[name] = tpl
			l.mu.Unlock()

			return tpl, nil
		}
	}

	return nil, fmt.Errorf("template %s not found in %v", name, l.dirs)
}

func (l *Loader) ClearCache() {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.cache = make(map[string]*pongo2.Template)
}

func (l *Loader) AddDir(dir string) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.dirs = append(l.dirs, dir)
}
