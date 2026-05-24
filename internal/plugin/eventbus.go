package plugin

import (
	"context"
	"sync"
)

type HookFunc func(ctx context.Context, data interface{}) error
type FilterFunc func(ctx context.Context, data interface{}) (interface{}, error)

type EventBus struct {
	hooks   map[string][]HookFunc
	filters map[string][]FilterFunc
	mu      sync.RWMutex
}

func NewEventBus() *EventBus {
	return &EventBus{
		hooks:   make(map[string][]HookFunc),
		filters: make(map[string][]FilterFunc),
	}
}

func (eb *EventBus) RegisterHook(name string, fn HookFunc) {
	eb.mu.Lock()
	defer eb.mu.Unlock()
	eb.hooks[name] = append(eb.hooks[name], fn)
}

func (eb *EventBus) RegisterFilter(name string, fn FilterFunc) {
	eb.mu.Lock()
	defer eb.mu.Unlock()
	eb.filters[name] = append(eb.filters[name], fn)
}

func (eb *EventBus) EmitHook(ctx context.Context, name string, data interface{}) error {
	eb.mu.RLock()
	hooks := eb.hooks[name]
	eb.mu.RUnlock()

	for _, fn := range hooks {
		if err := fn(ctx, data); err != nil {
			return err
		}
	}
	return nil
}

func (eb *EventBus) ApplyFilter(ctx context.Context, name string, data interface{}) (interface{}, error) {
	eb.mu.RLock()
	filters := eb.filters[name]
	eb.mu.RUnlock()

	result := data
	for _, fn := range filters {
		var err error
		result, err = fn(ctx, result)
		if err != nil {
			return nil, err
		}
	}
	return result, nil
}

const (
	HookPostBeforeCreate  = "post.before_create"
	HookPostAfterCreate   = "post.after_create"
	HookPostBeforeUpdate  = "post.before_update"
	HookPostAfterUpdate   = "post.after_update"
	HookPostBeforeDelete  = "post.before_delete"
	HookPostAfterDelete   = "post.after_delete"
	HookPostBeforePublish = "post.before_publish"
	HookPostAfterPublish  = "post.after_publish"

	HookCommentBeforeCreate = "comment.before_create"
	HookCommentAfterCreate  = "comment.after_create"

	HookUserBeforeCreate = "user.before_create"
	HookUserAfterCreate  = "user.after_create"
	HookUserAfterLogin   = "user.after_login"

	HookBeforeRender = "render.before"
	HookAfterRender  = "render.after"

	HookRequestStart = "request.start"
	HookRequestEnd   = "request.end"
)

const (
	FilterPostContent    = "post.content"
	FilterPostExcerpt    = "post.excerpt"
	FilterPostTitle      = "post.title"
	FilterCommentContent = "comment.content"
	FilterTemplateData   = "template.data"
)
