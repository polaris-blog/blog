package theme

import (
	"archive/zip"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"sync"

	"gopkg.in/yaml.v3"
)

type ThemeMeta struct {
	ID          string         `yaml:"id" json:"id"`
	Name        string         `yaml:"name" json:"name"`
	Version     string         `yaml:"version" json:"version"`
	Author      string         `yaml:"author" json:"author"`
	Description string         `yaml:"description" json:"description"`
	License     string         `yaml:"license" json:"license"`
	Homepage    string         `yaml:"homepage" json:"homepage"`
	Screenshot  string         `yaml:"screenshot" json:"screenshot"`
	Requires    ThemeRequires  `yaml:"requires" json:"requires"`
	Settings    []ThemeSetting `yaml:"settings" json:"settings"`
}

type ThemeRequires struct {
	MinVersion string `yaml:"min_version" json:"min_version"`
	MaxVersion string `yaml:"max_version" json:"max_version"`
}

type ThemeSetting struct {
	Key         string      `yaml:"key" json:"key"`
	Label       string      `yaml:"label" json:"label"`
	Type        string      `yaml:"type" json:"type"`
	Default     interface{} `yaml:"default" json:"default"`
	Options     []Option    `yaml:"options,omitempty" json:"options,omitempty"`
	Description string      `yaml:"description,omitempty" json:"description,omitempty"`
}

type Option struct {
	Label string `yaml:"label" json:"label"`
	Value string `yaml:"value" json:"value"`
}

type Theme struct {
	Meta     ThemeMeta
	Path     string
	Settings map[string]interface{}
}

type SettingsStore interface {
	Get(key string) (string, error)
	Set(key string, value string) error
}

type Manager struct {
	themes       map[string]*Theme
	activeTheme  string
	themeDir     string
	renderer     *Renderer
	settingsStore SettingsStore
	mu           sync.RWMutex
}

func NewManager(themeDir, activeTheme string, debug bool) *Manager {
	m := &Manager{
		themes:      make(map[string]*Theme),
		activeTheme: activeTheme,
		themeDir:    themeDir,
	}

	loader := NewLoader()
	m.renderer = NewRenderer(loader, debug)

	return m
}

func (m *Manager) SetSettingsStore(store SettingsStore) {
	m.settingsStore = store
}

func (m *Manager) LoadThemes() error {
	entries, err := os.ReadDir(m.themeDir)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("read theme dir: %w", err)
	}

	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}

		themePath := filepath.Join(m.themeDir, entry.Name())
		if err := m.loadTheme(themePath); err != nil {
			continue
		}
	}

	if _, ok := m.themes[m.activeTheme]; !ok {
		return fmt.Errorf("active theme %q not found", m.activeTheme)
	}

	m.updateLoaderDirs()

	return nil
}

func (m *Manager) loadTheme(path string) error {
	metaPath := filepath.Join(path, "theme.yaml")
	data, err := os.ReadFile(metaPath)
	if err != nil {
		return fmt.Errorf("read theme.yaml: %w", err)
	}

	var meta ThemeMeta
	if err := yaml.Unmarshal(data, &meta); err != nil {
		return fmt.Errorf("parse theme.yaml: %w", err)
	}

	if meta.ID == "" {
		meta.ID = filepath.Base(path)
	}

	settings := make(map[string]interface{})
	for _, s := range meta.Settings {
		settings[s.Key] = s.Default
	}

	theme := &Theme{
		Meta:     meta,
		Path:     path,
		Settings: settings,
	}

	m.mu.Lock()
	m.themes[meta.ID] = theme
	m.mu.Unlock()

	return nil
}

func (m *Manager) updateLoaderDirs() {
	var dirs []string

	if theme, ok := m.themes[m.activeTheme]; ok {
		dirs = append(dirs, filepath.Join(theme.Path, "templates"))
	}

	m.renderer.loader.dirs = dirs
	m.renderer.loader.ClearCache()
}

func (m *Manager) GetActiveTheme() *Theme {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.themes[m.activeTheme]
}

func (m *Manager) GetTheme(id string) (*Theme, bool) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	t, ok := m.themes[id]
	return t, ok
}

func (m *Manager) ListThemes() []*Theme {
	m.mu.RLock()
	defer m.mu.RUnlock()

	list := make([]*Theme, 0, len(m.themes))
	for _, t := range m.themes {
		list = append(list, t)
	}
	return list
}

func (m *Manager) SetActiveTheme(id string) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	if _, ok := m.themes[id]; !ok {
		return fmt.Errorf("theme %q not found", id)
	}

	m.activeTheme = id
	m.updateLoaderDirs()

	if m.settingsStore != nil {
		_ = m.settingsStore.Set("active_theme", id)
	}

	return nil
}

func (m *Manager) Renderer() *Renderer {
	return m.renderer
}

func (m *Manager) UpdateThemeSettings(id string, settings map[string]interface{}) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	theme, ok := m.themes[id]
	if !ok {
		return fmt.Errorf("theme %q not found", id)
	}

	for k, v := range settings {
		theme.Settings[k] = v
	}

	if m.settingsStore != nil {
		settingsJSON, err := json.Marshal(theme.Settings)
		if err != nil {
			return fmt.Errorf("marshal settings: %w", err)
		}
		if err := m.settingsStore.Set("theme_settings_"+id, string(settingsJSON)); err != nil {
			return fmt.Errorf("save settings: %w", err)
		}
	}

	return nil
}

func (m *Manager) LoadThemeSettings(id string) {
	if m.settingsStore == nil {
		return
	}

	m.mu.Lock()
	defer m.mu.Unlock()

	theme, ok := m.themes[id]
	if !ok {
		return
	}

	settingsJSON, err := m.settingsStore.Get("theme_settings_" + id)
	if err != nil || settingsJSON == "" {
		return
	}

	var saved map[string]interface{}
	if err := json.Unmarshal([]byte(settingsJSON), &saved); err != nil {
		return
	}

	for k, v := range saved {
		theme.Settings[k] = v
	}
}

func (m *Manager) StaticDir() string {
	m.mu.RLock()
	defer m.mu.RUnlock()

	if theme, ok := m.themes[m.activeTheme]; ok {
		return filepath.Join(theme.Path, "static")
	}
	return ""
}

func (m *Manager) ThemeDir() string {
	return m.themeDir
}

func (m *Manager) InstallFromZip(zipPath string) (*Theme, error) {
	r, err := zip.OpenReader(zipPath)
	if err != nil {
		return nil, fmt.Errorf("open zip: %w", err)
	}
	defer r.Close()

	var rootDir string
	for _, f := range r.File {
		name := f.Name
		if strings.Contains(name, "theme.yaml") {
			parts := strings.Split(name, "/")
			if len(parts) >= 2 {
				rootDir = parts[0]
			}
			break
		}
	}

	if rootDir == "" {
		return nil, fmt.Errorf("theme.yaml not found in zip")
	}

	if strings.Contains(rootDir, "..") {
		return nil, fmt.Errorf("invalid theme directory name")
	}

	destDir := filepath.Join(m.themeDir, rootDir)
	absDestDir, err := filepath.Abs(destDir)
	if err != nil {
		return nil, fmt.Errorf("resolve destination path: %w", err)
	}

	if _, err := os.Stat(destDir); err == nil {
		os.RemoveAll(destDir)
	}

	var totalSize int64
	for _, f := range r.File {
		if !strings.HasPrefix(f.Name, rootDir+"/") {
			continue
		}

		relPath := strings.TrimPrefix(f.Name, rootDir+"/")
		if relPath == "" {
			continue
		}

		if strings.Contains(relPath, "..") {
			return nil, fmt.Errorf("invalid path in zip: %s", relPath)
		}

		destPath := filepath.Join(destDir, relPath)
		absDestPath, err := filepath.Abs(destPath)
		if err != nil {
			return nil, fmt.Errorf("resolve path: %w", err)
		}

		if !strings.HasPrefix(absDestPath, absDestDir+string(os.PathSeparator)) && absDestPath != absDestDir {
			return nil, fmt.Errorf("path traversal detected: %s", relPath)
		}

		if f.FileInfo().IsDir() {
			os.MkdirAll(destPath, 0755)
			continue
		}

		if f.UncompressedSize64 > 50*1024*1024 {
			return nil, fmt.Errorf("file too large: %s", relPath)
		}
		totalSize += int64(f.UncompressedSize64)
		if totalSize > 100*1024*1024 {
			return nil, fmt.Errorf("zip total size exceeds limit")
		}

		os.MkdirAll(filepath.Dir(destPath), 0755)

		rc, err := f.Open()
		if err != nil {
			return nil, fmt.Errorf("extract %s: %w", f.Name, err)
		}

		outFile, err := os.Create(destPath)
		if err != nil {
			rc.Close()
			return nil, fmt.Errorf("create %s: %w", destPath, err)
		}

		_, err = io.CopyN(outFile, rc, 50*1024*1024)
		rc.Close()
		outFile.Close()
		if err != nil && err != io.EOF {
			return nil, fmt.Errorf("write %s: %w", destPath, err)
		}
	}

	if err := m.loadTheme(destDir); err != nil {
		os.RemoveAll(destDir)
		return nil, fmt.Errorf("invalid theme: %w", err)
	}

	theme := m.themes[m.themes[rootDir].Meta.ID]
	return theme, nil
}

func (m *Manager) DeleteTheme(id string) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	if id == m.activeTheme {
		return fmt.Errorf("cannot delete active theme")
	}

	theme, ok := m.themes[id]
	if !ok {
		return fmt.Errorf("theme %q not found", id)
	}

	if err := os.RemoveAll(theme.Path); err != nil {
		return fmt.Errorf("remove theme files: %w", err)
	}

	delete(m.themes, id)
	return nil
}
