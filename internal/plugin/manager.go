package plugin

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
)

type PluginSetting struct {
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

type PluginMeta struct {
	ID          string         `yaml:"id" json:"id"`
	Name        string         `yaml:"name" json:"name"`
	Version     string         `yaml:"version" json:"version"`
	Author      string         `yaml:"author" json:"author"`
	Description string         `yaml:"description" json:"description"`
	Hooks       []string       `yaml:"hooks" json:"hooks"`
	Filters     []string       `yaml:"filters" json:"filters"`
	Settings    []PluginSetting `yaml:"settings" json:"settings"`
}

type Plugin interface {
	Meta() PluginMeta
	Init(ctx context.Context, app AppContext) error
	Destroy(ctx context.Context) error
}

type AppContext interface {
	EmitHook(name string, data interface{}) error
	ApplyFilter(name string, data interface{}) (interface{}, error)
}

type SettingsStore interface {
	Get(key string) (string, error)
	Set(key string, value string) error
}

type Manager struct {
	eventBus      *EventBus
	plugins       map[string]Plugin
	wasmRT        *WasmRuntime
	pluginDir     string
	mu            sync.RWMutex
	appCtx        AppContext
	settingsStore SettingsStore
}

func NewManager(eventBus *EventBus) *Manager {
	return &Manager{
		eventBus: eventBus,
		plugins:  make(map[string]Plugin),
	}
}

func (m *Manager) SetSettingsStore(store SettingsStore) {
	m.settingsStore = store
}

func (m *Manager) InitWasm(ctx context.Context, cfg WasmConfig) error {
	m.wasmRT = NewWasmRuntime(ctx, m.eventBus, cfg, m)
	return nil
}

func (m *Manager) SetAppContext(ctx AppContext) {
	m.appCtx = ctx
}

func (m *Manager) SetPluginDir(dir string) {
	m.pluginDir = dir
}

func (m *Manager) Register(p Plugin) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	meta := p.Meta()
	if _, exists := m.plugins[meta.ID]; exists {
		return fmt.Errorf("plugin %s already registered", meta.ID)
	}

	if err := p.Init(context.Background(), m.appCtx); err != nil {
		return fmt.Errorf("init plugin %s: %w", meta.ID, err)
	}

	m.plugins[meta.ID] = p
	return nil
}

func (m *Manager) Unregister(id string) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	p, exists := m.plugins[id]
	if !exists {
		return fmt.Errorf("plugin %s not found", id)
	}

	if err := p.Destroy(context.Background()); err != nil {
		return fmt.Errorf("destroy plugin %s: %w", id, err)
	}

	delete(m.plugins, id)
	return nil
}

func (m *Manager) Get(id string) (Plugin, bool) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	p, ok := m.plugins[id]
	return p, ok
}

func (m *Manager) List() []PluginMeta {
	m.mu.RLock()
	defer m.mu.RUnlock()

	metas := make([]PluginMeta, 0, len(m.plugins))
	for _, p := range m.plugins {
		metas = append(metas, p.Meta())
	}
	return metas
}

func (m *Manager) GetPluginSettings(id string) (map[string]interface{}, bool) {
	m.mu.RLock()
	defer m.mu.RUnlock()

	if m.wasmRT == nil {
		return nil, false
	}
	return m.wasmRT.getPluginSettings(id)
}

func (m *Manager) UpdatePluginSettings(id string, settings map[string]interface{}) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	if m.wasmRT == nil {
		return fmt.Errorf("wasm runtime not initialized")
	}

	if err := m.wasmRT.updatePluginSettings(id, settings); err != nil {
		return err
	}

	if m.settingsStore != nil {
		currentSettings, _ := m.wasmRT.getPluginSettings(id)
		if currentSettings != nil {
			settingsJSON, _ := json.Marshal(currentSettings)
			_ = m.settingsStore.Set("plugin_settings_"+id, string(settingsJSON))
		}
	}

	return nil
}

func (m *Manager) LoadPluginSettings(id string) {
	if m.settingsStore == nil {
		return
	}

	settingsJSON, err := m.settingsStore.Get("plugin_settings_" + id)
	if err != nil || settingsJSON == "" {
		return
	}

	var saved map[string]interface{}
	if json.Unmarshal([]byte(settingsJSON), &saved) != nil {
		return
	}

	m.mu.Lock()
	defer m.mu.Unlock()

	if m.wasmRT != nil {
		m.wasmRT.updatePluginSettings(id, saved)
	}
}

func (m *Manager) EventBus() *EventBus {
	return m.eventBus
}

func (m *Manager) WasmRuntime() *WasmRuntime {
	return m.wasmRT
}

func (m *Manager) LoadFromDir(ctx context.Context) error {
	if m.pluginDir == "" {
		return nil
	}

	info, err := os.Stat(m.pluginDir)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("stat plugin dir: %w", err)
	}
	if !info.IsDir() {
		return nil
	}

	return filepath.Walk(m.pluginDir, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return nil
		}
		if info.IsDir() {
			return nil
		}
		if filepath.Ext(path) != ".wasm" {
			return nil
		}

		pluginID := strings.TrimSuffix(filepath.Base(path), ".wasm")

		wasmBytes, err := os.ReadFile(path)
		if err != nil {
			return fmt.Errorf("read plugin %s: %w", pluginID, err)
		}

		if err := m.LoadWasm(ctx, pluginID, wasmBytes); err != nil {
			return fmt.Errorf("load plugin %s: %w", pluginID, err)
		}

		return nil
	})
}

func (m *Manager) LoadWasm(ctx context.Context, id string, wasmBytes []byte) error {
	if m.wasmRT == nil {
		return fmt.Errorf("wasm runtime not initialized")
	}

	wp, err := m.wasmRT.Load(ctx, wasmBytes, id)
	if err != nil {
		return err
	}

	m.mu.Lock()
	m.plugins[id] = wp
	m.mu.Unlock()
	return nil
}

func (m *Manager) UnloadWasm(ctx context.Context, id string) error {
	if m.wasmRT == nil {
		return fmt.Errorf("wasm runtime not initialized")
	}

	if err := m.wasmRT.Unload(ctx, id); err != nil {
		return err
	}

	m.mu.Lock()
	delete(m.plugins, id)
	m.mu.Unlock()
	return nil
}

func (m *Manager) Close(ctx context.Context) error {
	if m.wasmRT != nil {
		return m.wasmRT.Close(ctx)
	}
	return nil
}
