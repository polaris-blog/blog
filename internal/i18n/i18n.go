package i18n

import (
	"embed"
	"encoding/json"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"sync"

	"gopkg.in/yaml.v3"
)

type Bundle struct {
	mu       sync.RWMutex
	locales  map[string]map[string]string
	defaults map[string]string
}

func NewBundle() *Bundle {
	return &Bundle{
		locales:  make(map[string]map[string]string),
		defaults: make(map[string]string),
	}
}

func (b *Bundle) LoadFromFS(fsys embed.FS, subdir string) error {
	sub, err := fs.Sub(fsys, subdir)
	if err != nil {
		return fmt.Errorf("sub fs %s: %w", subdir, err)
	}
	return fs.WalkDir(sub, ".", func(path string, d fs.DirEntry, err error) error {
		if err != nil || d.IsDir() {
			return nil
		}
		if !strings.HasSuffix(path, ".yaml") && !strings.HasSuffix(path, ".yml") {
			return nil
		}
		lang := strings.TrimSuffix(path, filepath.Ext(path))
		data, err := fs.ReadFile(sub, path)
		if err != nil {
			return fmt.Errorf("read %s: %w", path, err)
		}
		var translations map[string]string
		if err := yaml.Unmarshal(data, &translations); err != nil {
			return fmt.Errorf("parse %s: %w", path, err)
		}
		b.mu.Lock()
		if existing, ok := b.locales[lang]; ok {
			for k, v := range translations {
				existing[k] = v
			}
		} else {
			b.locales[lang] = translations
		}
		b.mu.Unlock()
		return nil
	})
}

func (b *Bundle) LoadFromDir(dir string) error {
	return filepath.WalkDir(dir, func(path string, d fs.DirEntry, err error) error {
		if err != nil || d.IsDir() {
			return nil
		}
		if !strings.HasSuffix(path, ".yaml") && !strings.HasSuffix(path, ".yml") {
			return nil
		}
		lang := strings.TrimSuffix(filepath.Base(path), filepath.Ext(path))
		data, err := os.ReadFile(path)
		if err != nil {
			return nil
		}
		var translations map[string]string
		if err := yaml.Unmarshal(data, &translations); err != nil {
			return nil
		}
		b.mu.Lock()
		if existing, ok := b.locales[lang]; ok {
			for k, v := range translations {
				existing[k] = v
			}
		} else {
			b.locales[lang] = translations
		}
		b.mu.Unlock()
		return nil
	})
}

func (b *Bundle) SetDefault(lang string) {
	b.mu.Lock()
	defer b.mu.Unlock()
	if m, ok := b.locales[lang]; ok {
		b.defaults = m
	}
}

func (b *Bundle) Languages() []string {
	b.mu.RLock()
	defer b.mu.RUnlock()
	var langs []string
	for k := range b.locales {
		langs = append(langs, k)
	}
	return langs
}

func (b *Bundle) T(lang, key string, args ...map[string]interface{}) string {
	b.mu.RLock()
	defer b.mu.RUnlock()

	val := ""
	if m, ok := b.locales[lang]; ok {
		val = m[key]
	}
	if val == "" {
		val = b.defaults[key]
	}
	if val == "" {
		val = key
	}

	if len(args) > 0 {
		for k, v := range args[0] {
			val = strings.ReplaceAll(val, "%"+k+"%", fmt.Sprintf("%v", v))
		}
	}

	return val
}

func (b *Bundle) AllTranslations(lang string) map[string]string {
	b.mu.RLock()
	defer b.mu.RUnlock()

	result := make(map[string]string, len(b.defaults))
	for k, v := range b.defaults {
		result[k] = v
	}
	if m, ok := b.locales[lang]; ok {
		for k, v := range m {
			result[k] = v
		}
	}
	return result
}

func (b *Bundle) ToJSON(lang string) ([]byte, error) {
	translations := b.AllTranslations(lang)
	return json.Marshal(translations)
}
