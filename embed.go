package blog

import "embed"

//go:embed all:web/admin/templates
var AdminTemplates embed.FS

//go:embed all:web/admin/static
var AdminStatic embed.FS

//go:embed all:themes/default
var DefaultTheme embed.FS

//go:embed all:configs
var Configs embed.FS

//go:embed all:plugins
var Plugins embed.FS
