<div align="center">

# ⭐ Polaris

**A lightweight, self-contained blog engine powered by Go**

[English](#english) · [中文](#中文)

</div>

---

<a id="english"></a>

## Features

- **Single Binary** — All templates, static assets, default theme, and plugins are embedded via `embed.FS`. Deploy with just one file.
- **Web Setup Wizard** — First run launches an installation guide (database, site info, admin account). No manual config editing needed.
- **Multi-Database** — SQLite (default), MySQL, PostgreSQL
- **Markdown** — Goldmark engine with GFM, footnotes, syntax highlighting (Chroma), line numbers
- **Theme System** — Pongo2 (Jinja2-style) templates, ZIP upload, per-theme settings, live preview
- **Plugin System** — WASM plugins via wazero (pure Go, no CGO), Hook & Filter mechanism, ZIP upload
- **i18n** — English & Chinese built-in, auto-detect browser language, easily extensible
- **RSS & Sitemap** — RSS 2.0 feed at `/feed.xml`, XML Sitemap at `/sitemap.xml`
- **Full-Text Search** — Bleve (built-in) or Meilisearch
- **Media Management** — Upload, organize by date, URL copy
- **Comment System** — Visitor comments, admin approval, spam marking, nested replies
- **Admin Dashboard** — Clean, responsive UI built with Alpine.js + HTMX + Tailwind CSS
- **Security** — JWT auth, CSRF protection, security headers, bcrypt passwords, path traversal prevention

## Quick Start

### Download

Download the latest release from [Releases](https://github.com/polaris-blog/blog/releases).

### Run

```bash
./polaris
```

Open `http://localhost:8080` and follow the setup wizard.

### Command Line

```bash
polaris [data-dir] [config-path]
# data-dir:     default .polaris
# config-path:  default <data-dir>/configs/default.yaml
```

### Environment Variables

| Variable | Description |
|----------|-------------|
| `POLARIS_ADDR` | Listen address (default `:8080`) |
| `POLARIS_DB_DRIVER` | Database driver: `sqlite`, `mysql`, `postgres` |
| `POLARIS_DB_DSN` | Database connection string |
| `POLARIS_SECRET_KEY` | Secret key for JWT & sessions |
| `POLARIS_MODE` | Run mode: `release`, `debug` |

### Docker

```bash
docker run -d \
  -p 8080:8080 \
  -v polaris-data:/app/.polaris \
  ghcr.io/polaris-blog/polaris:latest
```

## Build from Source

```bash
git clone https://github.com/polaris-blog/blog.git
cd blog
go build -o polaris ./cmd/polaris
```

> Requires Go 1.23+. No CGO required.

## Configuration

Default config is embedded and extracted to `.polaris/configs/default.yaml` on first run.

```yaml
server:
  addr: ":8080"
  mode: "release"

database:
  driver: "sqlite"    # sqlite | mysql | postgres
  dsn: "polaris.db"

theme:
  active: "default"

plugin:
  dir: "./plugins"
  wasm:
    max_memory_mb: 32
    timeout_seconds: 10

i18n:
  default: "en"       # en | zh
```

## Project Structure

```
├── cmd/polaris/          # Entry point
├── configs/              # Default config & locales
│   └── locales/          # i18n translations (en.yaml, zh.yaml)
├── internal/
│   ├── app/              # App init, self-extract, setup wizard
│   ├── config/           # Config loading
│   ├── database/         # Database abstraction
│   ├── http/             # HTTP handlers & middleware
│   ├── i18n/             # Internationalization
│   ├── model/            # Data models
│   ├── plugin/           # WASM plugin system
│   ├── repository/       # Data access layer
│   ├── service/          # Business logic
│   ├── storage/          # File storage
│   └── theme/            # Theme engine
├── plugins/              # Built-in WASM plugins
├── themes/default/       # Default theme
└── web/admin/            # Admin panel (templates + static)
```

## Plugin Development

Polaris uses WASM plugins (via wazero, pure Go runtime). Plugins can register hooks and filters:

**Available Hooks:** `post.before_create`, `post.after_create`, `post.before_update`, `post.after_update`, `post.before_delete`, `post.after_delete`, `post.before_publish`, `post.after_publish`, `comment.before_create`, `comment.after_create`, `user.after_login`, `render.before`, `render.after`

**Available Filters:** `post.content`, `post.excerpt`, `post.title`, `comment.content`, `template.data`

See `plugins/github-card/` for a complete example.

## Theme Development

Themes use Pongo2 (Jinja2-style) templates. A theme directory should contain:

```
my-theme/
├── theme.yaml            # Theme metadata & settings
├── templates/
│   ├── base.html         # Base layout
│   ├── index.html        # Home page
│   ├── post.html         # Single post
│   ├── page.html         # Custom page
│   ├── archive.html      # Archives
│   ├── categories.html   # Category list
│   ├── tags.html         # Tag list
│   └── partials/         # Shared partials
└── static/               # Theme assets
```

## License

MIT License

---

<a id="中文"></a>

## 特性

- **单文件部署** — 所有模板、静态资源、默认主题和插件通过 `embed.FS` 嵌入，一个二进制文件即可运行
- **Web 安装向导** — 首次运行自动启动安装引导（数据库配置、站点信息、管理员创建），无需手动编辑配置
- **多数据库** — SQLite（默认）、MySQL、PostgreSQL
- **Markdown** — Goldmark 引擎，支持 GFM、脚注、代码高亮（Chroma）、行号显示
- **主题系统** — Pongo2（Jinja2 风格）模板引擎，ZIP 上传安装，主题设置持久化
- **插件系统** — 基于 wazero 的 WASM 插件（纯 Go，无 CGO 依赖），Hook & Filter 机制，ZIP 上传安装
- **国际化** — 内置英文和中文，自动检测浏览器语言，可轻松扩展
- **RSS & Sitemap** — `/feed.xml` 输出 RSS 2.0，`/sitemap.xml` 输出 XML Sitemap
- **全文搜索** — Bleve（内置）或 Meilisearch
- **媒体管理** — 文件上传、按日期归档、URL 复制
- **评论系统** — 访客评论、管理员审核、垃圾评论标记、嵌套回复
- **管理后台** — 基于 Alpine.js + HTMX + Tailwind CSS 的简洁响应式管理界面
- **安全** — JWT 认证、CSRF 防护、安全响应头、bcrypt 密码加密、路径遍历防护

## 快速开始

### 下载

从 [Releases](https://github.com/polaris-blog/blog/releases) 下载最新版本。

### 运行

```bash
./polaris
```

打开 `http://localhost:8080`，按照安装向导操作即可。

### 命令行参数

```bash
polaris [数据目录] [配置文件路径]
# 数据目录:     默认 .polaris
# 配置文件路径: 默认 <数据目录>/configs/default.yaml
```

### 环境变量

| 变量 | 说明 |
|------|------|
| `POLARIS_ADDR` | 监听地址（默认 `:8080`） |
| `POLARIS_DB_DRIVER` | 数据库驱动：`sqlite`、`mysql`、`postgres` |
| `POLARIS_DB_DSN` | 数据库连接字符串 |
| `POLARIS_SECRET_KEY` | JWT 和会话的密钥 |
| `POLARIS_MODE` | 运行模式：`release`、`debug` |

### Docker

```bash
docker run -d \
  -p 8080:8080 \
  -v polaris-data:/app/.polaris \
  ghcr.io/polaris-blog/polaris:latest
```

## 从源码编译

```bash
git clone https://github.com/polaris-blog/blog.git
cd blog
go build -o polaris ./cmd/polaris
```

> 需要 Go 1.23+，无需 CGO。

## 配置

默认配置在首次运行时嵌入并释放到 `.polaris/configs/default.yaml`。

```yaml
server:
  addr: ":8080"
  mode: "release"

database:
  driver: "sqlite"    # sqlite | mysql | postgres
  dsn: "polaris.db"

theme:
  active: "default"

plugin:
  dir: "./plugins"
  wasm:
    max_memory_mb: 32
    timeout_seconds: 10

i18n:
  default: "en"       # en | zh
```

## 项目结构

```
├── cmd/polaris/          # 程序入口
├── configs/              # 默认配置与翻译文件
│   └── locales/          # 国际化翻译 (en.yaml, zh.yaml)
├── internal/
│   ├── app/              # 应用初始化、自释放、安装向导
│   ├── config/           # 配置加载
│   ├── database/         # 数据库抽象层
│   ├── http/             # HTTP 处理器与中间件
│   ├── i18n/             # 国际化
│   ├── model/            # 数据模型
│   ├── plugin/           # WASM 插件系统
│   ├── repository/       # 数据访问层
│   ├── service/          # 业务逻辑层
│   ├── storage/          # 文件存储
│   └── theme/            # 主题引擎
├── plugins/              # 内置 WASM 插件
├── themes/default/       # 默认主题
└── web/admin/            # 管理后台（模板 + 静态资源）
```

## 插件开发

Polaris 使用 WASM 插件（基于 wazero 纯 Go 运行时）。插件可以注册钩子和过滤器：

**可用钩子：** `post.before_create`、`post.after_create`、`post.before_update`、`post.after_update`、`post.before_delete`、`post.after_delete`、`post.before_publish`、`post.after_publish`、`comment.before_create`、`comment.after_create`、`user.after_login`、`render.before`、`render.after`

**可用过滤器：** `post.content`、`post.excerpt`、`post.title`、`comment.content`、`template.data`

完整示例请参考 `plugins/github-card/`。

## 主题开发

主题使用 Pongo2（Jinja2 风格）模板。主题目录结构：

```
my-theme/
├── theme.yaml            # 主题元数据与设置
├── templates/
│   ├── base.html         # 基础布局
│   ├── index.html        # 首页
│   ├── post.html         # 文章页
│   ├── page.html         # 自定义页面
│   ├── archive.html      # 归档页
│   ├── categories.html   # 分类列表
│   ├── tags.html         # 标签列表
│   └── partials/         # 公共模板片段
└── static/               # 主题静态资源
```

## 许可证

MIT License
