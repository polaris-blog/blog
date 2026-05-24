package plugin

import (
	"context"
)

type WasmPlugin struct {
	id      string
	module  *wasmModule
	runtime *WasmRuntime
}

func (p *WasmPlugin) Meta() PluginMeta {
	return p.module.meta
}

func (p *WasmPlugin) Init(ctx context.Context, appCtx AppContext) error {
	return nil
}

func (p *WasmPlugin) Destroy(ctx context.Context) error {
	return p.runtime.Unload(ctx, p.id)
}
