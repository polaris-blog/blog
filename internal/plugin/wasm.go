package plugin

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"sync"
	"time"

	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/api"
)

type WasmRuntime struct {
	runtime  wazero.Runtime
	host     *wasmHost
	modules  map[string]*wasmModule
	mu       sync.RWMutex
	maxMemMB int
	manager  *Manager
	hostInit sync.Once
}

type wasmModule struct {
	compiled wazero.CompiledModule
	instance api.Module
	meta     PluginMeta
	hooks    map[string]uint32
	filters  map[string]uint32
	settings map[string]interface{}
	settingsMu sync.RWMutex
}

type WasmConfig struct {
	MaxMemoryMB    int `yaml:"max_memory_mb"`
	TimeoutSeconds int `yaml:"timeout_seconds"`
}

func NewWasmRuntime(ctx context.Context, eventBus *EventBus, cfg WasmConfig, manager *Manager) *WasmRuntime {
	if cfg.MaxMemoryMB <= 0 {
		cfg.MaxMemoryMB = 32
	}
	if cfg.TimeoutSeconds <= 0 {
		cfg.TimeoutSeconds = 10
	}

	rt := wazero.NewRuntime(ctx)
	host := &wasmHost{
		eventBus:   eventBus,
		runtime:    rt,
		pending:    make(map[uint32]*pendingRegistration),
		nextID:     1,
		timeoutSec: cfg.TimeoutSeconds,
		manager:    manager,
	}

	wr := &WasmRuntime{
		runtime:  rt,
		host:     host,
		modules:  make(map[string]*wasmModule),
		maxMemMB: cfg.MaxMemoryMB,
		manager:  manager,
	}

	wr.initHostModules(ctx)

	return wr
}

func (r *WasmRuntime) initHostModules(ctx context.Context) {
	envBuilder := r.runtime.NewHostModuleBuilder("env")
	envBuilder.NewFunctionBuilder().
		WithGoModuleFunction(api.GoModuleFunc(func(ctx context.Context, m api.Module, stack []uint64) {
		}), []api.ValueType{api.ValueTypeI32, api.ValueTypeI32, api.ValueTypeI32, api.ValueTypeI32}, []api.ValueType{}).
		WithParameterNames("msg", "file", "line", "col").Export("abort")
	if _, err := envBuilder.Instantiate(ctx); err != nil {
		panic(fmt.Sprintf("instantiate env module: %v", err))
	}

	builder := r.runtime.NewHostModuleBuilder("polaris")
	builder.NewFunctionBuilder().
		WithGoModuleFunction(api.GoModuleFunc(func(ctx context.Context, m api.Module, stack []uint64) {
			result := r.host.hostRegisterHook(m, stack)
			stack[0] = result
		}), []api.ValueType{api.ValueTypeI32, api.ValueTypeI32}, []api.ValueType{api.ValueTypeI32}).
		WithParameterNames("name_ptr", "name_len").WithResultNames("id").Export("register_hook")
	builder.NewFunctionBuilder().
		WithGoModuleFunction(api.GoModuleFunc(func(ctx context.Context, m api.Module, stack []uint64) {
			result := r.host.hostRegisterFilter(m, stack)
			stack[0] = result
		}), []api.ValueType{api.ValueTypeI32, api.ValueTypeI32}, []api.ValueType{api.ValueTypeI32}).
		WithParameterNames("name_ptr", "name_len").WithResultNames("id").Export("register_filter")
	builder.NewFunctionBuilder().
		WithGoModuleFunction(api.GoModuleFunc(func(ctx context.Context, m api.Module, stack []uint64) {
			r.host.hostLog(m, stack)
		}), []api.ValueType{api.ValueTypeI32, api.ValueTypeI32}, []api.ValueType{}).
		WithParameterNames("msg_ptr", "msg_len").Export("log")
	builder.NewFunctionBuilder().
		WithGoModuleFunction(api.GoModuleFunc(func(ctx context.Context, m api.Module, stack []uint64) {
			r.host.hostGetConfig(m, stack)
		}), []api.ValueType{api.ValueTypeI32, api.ValueTypeI32, api.ValueTypeI32, api.ValueTypeI32}, []api.ValueType{api.ValueTypeI32}).
		WithParameterNames("key_ptr", "key_len", "buf_ptr", "buf_len").WithResultNames("value_len").Export("get_config")
	builder.NewFunctionBuilder().
		WithGoModuleFunction(api.GoModuleFunc(func(ctx context.Context, m api.Module, stack []uint64) {
			r.host.hostHttpGet(m, stack)
		}), []api.ValueType{api.ValueTypeI32, api.ValueTypeI32, api.ValueTypeI32, api.ValueTypeI32}, []api.ValueType{api.ValueTypeI32}).
		WithParameterNames("url_ptr", "url_len", "buf_ptr", "buf_len").WithResultNames("written").Export("http_get")

	if _, err := builder.Instantiate(ctx); err != nil {
		panic(fmt.Sprintf("instantiate polaris module: %v", err))
	}
}

func (r *WasmRuntime) Load(ctx context.Context, wasmBytes []byte, pluginID string) (*WasmPlugin, error) {
	compiled, err := r.runtime.CompileModule(ctx, wasmBytes)
	if err != nil {
		return nil, fmt.Errorf("compile wasm: %w", err)
	}

	inst, err := r.runtime.InstantiateModule(ctx, compiled, wazero.NewModuleConfig().
		WithName(pluginID).
		WithStartFunctions("_start"))
	if err != nil {
		compiled.Close(ctx)
		return nil, fmt.Errorf("instantiate wasm module: %w", err)
	}

	settings := make(map[string]interface{})

	mod := &wasmModule{
		compiled: compiled,
		instance: inst,
		meta:     PluginMeta{ID: pluginID},
		hooks:    make(map[string]uint32),
		filters:  make(map[string]uint32),
		settings: settings,
	}

	initFn := inst.ExportedFunction("polaris_init")
	if initFn != nil {
		if _, err = initFn.Call(ctx); err != nil {
			inst.Close(ctx)
			compiled.Close(ctx)
			return nil, fmt.Errorf("call polaris_init: %w", err)
		}
	}

	metaFn := inst.ExportedFunction("polaris_meta")
	if metaFn != nil {
		if results, err := metaFn.Call(ctx); err == nil && len(results) > 0 {
			ptr := uint32(results[0])
			mem := inst.Memory()
			if mem != nil {
				metaBytes := readNullTerminated(mem, ptr, 4096)
				if metaBytes != nil {
					var meta PluginMeta
					if json.Unmarshal(metaBytes, &meta) == nil {
						if meta.ID == "" {
							meta.ID = pluginID
						}
						mod.meta = meta
						for _, s := range meta.Settings {
							mod.settings[s.Key] = s.Default
						}
					}
				}
			}
		}
	}

	r.host.mu.Lock()
	for id, reg := range r.host.pending {
		if reg.pluginID == "" {
			reg.pluginID = pluginID
		}
		switch reg.kind {
		case "hook":
			mod.hooks[reg.name] = reg.callbackID
			hookName := reg.name
			cbID := reg.callbackID
			r.host.eventBus.RegisterHook(hookName, func(ctx context.Context, data interface{}) error {
				return r.callHook(ctx, pluginID, cbID, data)
			})
		case "filter":
			mod.filters[reg.name] = reg.callbackID
			filterName := reg.name
			cbID := reg.callbackID
			r.host.eventBus.RegisterFilter(filterName, func(ctx context.Context, data interface{}) (interface{}, error) {
				return r.callFilter(ctx, pluginID, cbID, data)
			})
		}
		delete(r.host.pending, id)
	}
	r.host.mu.Unlock()

	r.mu.Lock()
	r.modules[pluginID] = mod
	r.mu.Unlock()

	return &WasmPlugin{
		id:      pluginID,
		module:  mod,
		runtime: r,
	}, nil
}

func (r *WasmRuntime) getPluginSettings(pluginID string) (map[string]interface{}, bool) {
	r.mu.RLock()
	mod, ok := r.modules[pluginID]
	r.mu.RUnlock()
	if !ok {
		return nil, false
	}
	mod.settingsMu.RLock()
	defer mod.settingsMu.RUnlock()
	settingsCopy := make(map[string]interface{}, len(mod.settings))
	for k, v := range mod.settings {
		settingsCopy[k] = v
	}
	return settingsCopy, true
}

func (r *WasmRuntime) updatePluginSettings(pluginID string, settings map[string]interface{}) error {
	r.mu.RLock()
	mod, ok := r.modules[pluginID]
	r.mu.RUnlock()
	if !ok {
		return fmt.Errorf("plugin %s not found", pluginID)
	}
	mod.settingsMu.Lock()
	defer mod.settingsMu.Unlock()
	for k, v := range settings {
		mod.settings[k] = v
	}
	return nil
}

func (r *WasmRuntime) callHook(ctx context.Context, pluginID string, callbackID uint32, data interface{}) (err error) {
	defer func() {
		if r := recover(); r != nil {
			err = fmt.Errorf("plugin hook panic: %v", r)
		}
	}()

	r.mu.RLock()
	mod, ok := r.modules[pluginID]
	r.mu.RUnlock()
	if !ok {
		return fmt.Errorf("plugin %s not found", pluginID)
	}

	fn := mod.instance.ExportedFunction("polaris_on_hook")
	if fn == nil {
		return nil
	}

	dataBytes, marshalErr := json.Marshal(data)
	if marshalErr != nil {
		return fmt.Errorf("marshal hook data: %w", marshalErr)
	}
	mem := mod.instance.Memory()
	if mem == nil {
		return nil
	}

	ptr, err := r.allocate(ctx, mod.instance, uint32(len(dataBytes)))
	if err != nil {
		return err
	}
	mem.Write(ptr, dataBytes)

	timeoutCtx, cancel := context.WithTimeout(ctx, time.Duration(r.host.timeoutSec)*time.Second)
	defer cancel()

	_, err = fn.Call(timeoutCtx, uint64(callbackID), uint64(ptr), uint64(len(dataBytes)))
	return err
}

func readNullTerminated(mem api.Memory, ptr uint32, maxLen uint32) []byte {
	data, ok := mem.Read(ptr, maxLen)
	if !ok {
		return nil
	}
	for i, b := range data {
		if b == 0 {
			return data[:i]
		}
	}
	return data
}

func (r *WasmRuntime) callFilter(ctx context.Context, pluginID string, callbackID uint32, data interface{}) (result interface{}, err error) {
	defer func() {
		if r := recover(); r != nil {
			err = fmt.Errorf("plugin filter panic: %v", r)
			result = data
		}
	}()

	r.mu.RLock()
	mod, ok := r.modules[pluginID]
	r.mu.RUnlock()
	if !ok {
		return data, fmt.Errorf("plugin %s not found", pluginID)
	}

	fn := mod.instance.ExportedFunction("polaris_on_filter")
	if fn == nil {
		return data, nil
	}

	var dataBytes []byte
	switch v := data.(type) {
	case string:
		dataBytes = []byte(v)
	default:
		var marshalErr error
		dataBytes, marshalErr = json.Marshal(data)
		if marshalErr != nil {
			return data, fmt.Errorf("marshal filter data: %w", marshalErr)
		}
	}

	mem := mod.instance.Memory()
	if mem == nil {
		return data, nil
	}

	ptr, err := r.allocate(ctx, mod.instance, uint32(len(dataBytes)))
	if err != nil {
		return data, err
	}
	mem.Write(ptr, dataBytes)

	timeoutCtx, cancel := context.WithTimeout(ctx, time.Duration(r.host.timeoutSec)*time.Second)
	defer cancel()

	results, err := fn.Call(timeoutCtx, uint64(callbackID), uint64(ptr), uint64(len(dataBytes)))
	if err != nil {
		return data, err
	}

	if len(results) > 0 {
		resultPtr := uint32(results[0])
		resultBytes := readNullTerminated(mem, resultPtr, 65536)
		if resultBytes != nil {
			if _, ok := data.(string); ok {
				return string(resultBytes), nil
			}
			var jsonResult interface{}
			if json.Unmarshal(resultBytes, &jsonResult) == nil {
				return jsonResult, nil
			}
			return string(resultBytes), nil
		}
	}

	return data, nil
}

func (r *WasmRuntime) allocate(ctx context.Context, inst api.Module, size uint32) (uint32, error) {
	allocFn := inst.ExportedFunction("polaris_alloc")
	if allocFn == nil {
		allocFn = inst.ExportedFunction("allocate")
	}
	if allocFn == nil {
		return 0, fmt.Errorf("no allocator function found")
	}

	results, err := allocFn.Call(ctx, uint64(size))
	if err != nil {
		return 0, fmt.Errorf("allocate memory: %w", err)
	}
	if len(results) == 0 {
		return 0, fmt.Errorf("allocate returned no result")
	}
	return uint32(results[0]), nil
}

func (r *WasmRuntime) Unload(ctx context.Context, pluginID string) error {
	r.mu.Lock()
	defer r.mu.Unlock()

	mod, ok := r.modules[pluginID]
	if !ok {
		return fmt.Errorf("plugin %s not found", pluginID)
	}

	mod.instance.Close(ctx)
	mod.compiled.Close(ctx)
	delete(r.modules, pluginID)
	return nil
}

func (r *WasmRuntime) Close(ctx context.Context) error {
	r.mu.Lock()
	defer r.mu.Unlock()

	for id, mod := range r.modules {
		mod.instance.Close(ctx)
		mod.compiled.Close(ctx)
		delete(r.modules, id)
	}
	return r.runtime.Close(ctx)
}

func (r *WasmRuntime) ListModules() []PluginMeta {
	r.mu.RLock()
	defer r.mu.RUnlock()

	metas := make([]PluginMeta, 0, len(r.modules))
	for _, mod := range r.modules {
		metas = append(metas, mod.meta)
	}
	return metas
}

type pendingRegistration struct {
	kind       string
	name       string
	callbackID uint32
	pluginID   string
}

type wasmHost struct {
	eventBus   *EventBus
	runtime    wazero.Runtime
	pending    map[uint32]*pendingRegistration
	nextID     uint32
	mu         sync.Mutex
	timeoutSec int
	manager    *Manager
}

func (h *wasmHost) hostRegisterHook(m api.Module, stack []uint64) uint64 {
	namePtr := uint32(stack[0])
	nameLen := uint32(stack[1])

	nameBytes, ok := m.Memory().Read(namePtr, nameLen)
	if !ok {
		return 0
	}
	name := string(nameBytes)

	h.mu.Lock()
	defer h.mu.Unlock()

	id := h.nextID
	h.nextID++

	h.pending[id] = &pendingRegistration{
		kind:       "hook",
		name:       name,
		callbackID: id,
	}

	return uint64(id)
}

func (h *wasmHost) hostRegisterFilter(m api.Module, stack []uint64) uint64 {
	namePtr := uint32(stack[0])
	nameLen := uint32(stack[1])

	nameBytes, ok := m.Memory().Read(namePtr, nameLen)
	if !ok {
		return 0
	}
	name := string(nameBytes)

	h.mu.Lock()
	defer h.mu.Unlock()

	id := h.nextID
	h.nextID++

	h.pending[id] = &pendingRegistration{
		kind:       "filter",
		name:       name,
		callbackID: id,
	}

	return uint64(id)
}

func (h *wasmHost) hostLog(m api.Module, stack []uint64) {
	msgPtr := uint32(stack[0])
	msgLen := uint32(stack[1])

	msgBytes, ok := m.Memory().Read(msgPtr, msgLen)
	if !ok {
		return
	}
	fmt.Printf("[plugin] %s\n", string(msgBytes))
}

func (h *wasmHost) hostGetConfig(m api.Module, stack []uint64) {
	defer func() {
		if r := recover(); r != nil {
			stack[0] = 0
		}
	}()

	keyPtr := uint32(stack[0])
	keyLen := uint32(stack[1])
	bufPtr := uint32(stack[2])
	bufLen := uint32(stack[3])

	stack[0] = 0

	keyBytes, ok := m.Memory().Read(keyPtr, keyLen)
	if !ok {
		return
	}
	key := string(keyBytes)

	pluginName := m.Name()

	h.manager.wasmRT.mu.RLock()
	mod, exists := h.manager.wasmRT.modules[pluginName]
	h.manager.wasmRT.mu.RUnlock()

	if !exists {
		return
	}

	mod.settingsMu.RLock()
	val, exists := mod.settings[key]
	mod.settingsMu.RUnlock()

	if !exists {
		return
	}

	var valStr string
	switch v := val.(type) {
	case string:
		valStr = v
	case bool:
		if v {
			valStr = "true"
		} else {
			valStr = "false"
		}
	case float64:
		valStr = fmt.Sprintf("%v", v)
	case json.Number:
		valStr = v.String()
	default:
		b, _ := json.Marshal(v)
		valStr = string(b)
	}

	valBytes := []byte(valStr)
	copyLen := uint32(len(valBytes))
	if copyLen > bufLen {
		copyLen = bufLen
	}
	if copyLen == 0 {
		return
	}

	mem, ok := m.Memory().Read(bufPtr, copyLen)
	if !ok {
		return
	}
	copy(mem, valBytes[:copyLen])
	stack[0] = uint64(copyLen)
}

func (h *wasmHost) hostHttpGet(m api.Module, stack []uint64) {
	defer func() {
		if r := recover(); r != nil {
			stack[0] = 0
		}
	}()

	urlPtr := uint32(stack[0])
	urlLen := uint32(stack[1])
	bufPtr := uint32(stack[2])
	bufLen := uint32(stack[3])

	stack[0] = 0

	urlBytes, ok := m.Memory().Read(urlPtr, urlLen)
	if !ok {
		return
	}
	url := string(urlBytes)

	if len(url) > 2048 {
		return
	}
	if !strings.HasPrefix(url, "https://api.github.com/") {
		return
	}

	client := &http.Client{Timeout: 5 * time.Second}
	req, err := http.NewRequestWithContext(context.Background(), "GET", url, nil)
	if err != nil {
		return
	}
	req.Header.Set("User-Agent", "Polaris-Blog")

	pluginName := m.Name()
	h.manager.wasmRT.mu.RLock()
	mod, exists := h.manager.wasmRT.modules[pluginName]
	if exists {
		mod.settingsMu.RLock()
		if token, ok := mod.settings["github_token"]; ok {
			if tokenStr, ok := token.(string); ok && tokenStr != "" {
				req.Header.Set("Authorization", "token "+tokenStr)
			}
		}
		mod.settingsMu.RUnlock()
	}
	h.manager.wasmRT.mu.RUnlock()

	resp, err := client.Do(req)
	if err != nil {
		return
	}
	defer resp.Body.Close()

	body, err := io.ReadAll(io.LimitReader(resp.Body, int64(bufLen)))
	if err != nil {
		return
	}

	copyLen := uint32(len(body))
	if copyLen > bufLen {
		copyLen = bufLen
	}
	mem, ok := m.Memory().Read(bufPtr, copyLen)
	if !ok {
		return
	}
	copy(mem, body[:copyLen])
	stack[0] = uint64(copyLen)
}
