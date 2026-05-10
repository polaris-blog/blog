<div align="center">

# Polaris

**A minimalist, full-stack blog engine built with Go.**

极简主义全栈博客引擎

[![Go](https://img.shields.io/badge/Go-1.24-00ADD8?logo=go)](https://go.dev)
[![Docker](https://img.shields.io/badge/Docker-Ready-2496ED?logo=docker)](https://www.docker.com)
[![License](https://img.shields.io/badge/License-MIT-green)](LICENSE)

[English](#overview) · [中文](#概览)

</div>

---

## Overview

Polaris is a self-hosted blog platform focused on simplicity and performance. No Node.js, no build tools — just a single Go binary with embedded templates. Server-side rendered, dark mode, pluggable themes, and WASM plugin support.

## 概览

Polaris 是一个专注简洁与性能的自托管博客平台。无需 Node.js、无需构建工具——只需一个 Go 二进制文件，模板内嵌。服务端渲染、暗黑模式、可插拔主题、WASM 插件支持。

---

## ✨ Features / 功能特性

| | English | 中文 |
|---|---------|------|
| 🚀 | **Zero Node.js** — SSR with Go templates + TailwindCSS CDN + Alpine.js CDN | **零 Node.js** — Go 模板 + TailwindCSS CDN + Alpine.js CDN 服务端渲染 |
| 🗄️ | **Multi-Database** — SQLite (dev), MySQL / PostgreSQL (production) | **多数据库** — SQLite（开发）、MySQL / PostgreSQL（生产） |
| 🎨 | **Pluggable Themes** — Upload ZIP, hot-swap without restart | **可插拔主题** — ZIP 上传，热切换无需重启 |
| 🔌 | **WASM Plugins** — Extend with WebAssembly plugins | **WASM 插件** — WebAssembly 插件扩展 |
| 📝 | **Markdown + GFM** — Goldmark with auto heading IDs | **Markdown + GFM** — Goldmark 渲染，自动标题锚点 |
| 🔐 | **Security** — JWT, bcrypt, rate limiting, XSS/CSRF protection | **安全** — JWT、bcrypt、速率限制、XSS/CSRF 防护 |
| 🐳 | **Docker Ready** — Multi-arch (amd64/arm64), compose in one command | **Docker 就绪** — 多架构镜像，一键 compose |
| 🧙 | **Install Wizard** — Web-based setup with DB connection test | **安装向导** — Web 端安装，支持数据库连接测试 |
| 🌙 | **Dark Mode** — System preference detection, toggle with one click | **暗黑模式** — 自动检测系统偏好，一键切换 |

---

## 🚀 Quick Start / 快速开始

### Binary / 二进制

```bash
go build -o polaris ./cmd/server
./polaris
```

Open `http://localhost:3000/install` to complete setup.

打开 `http://localhost:3000/install` 完成安装。

### Docker

```bash
docker-compose up -d
```

Starts Polaris + MySQL 8.0 + Redis 7.

启动 Polaris + MySQL 8.0 + Redis 7。

---

## ⚙️ Configuration / 配置

Set `POLARIS_CONFIG` env var to override the default `config.yaml` path.

设置环境变量 `POLARIS_CONFIG` 可覆盖默认的 `config.yaml` 路径。

```yaml
server:
  host: 0.0.0.0
  port: 3000

database:
  driver: sqlite        # sqlite | mysql | postgres
  host: localhost
  port: 3306
  user: root
  password: ""
  dbname: polaris

auth:
  jwt_secret: change-this-in-production
  token_expiry: 168h
```

---

## 📁 Architecture / 架构

```
cmd/server/main.go           # Entry / 入口
internal/
  config/                     # YAML config / 配置
  database/                   # GORM connect + migrate / 数据库
  middleware/                 # JWT auth / 认证中间件
  models/                     # GORM models / 数据模型
  handlers/                   # HTTP handlers / 请求处理
  services/                   # Business logic / 业务逻辑
  server/                     # Fiber app + routes / 路由与渲染
  embed/                      # Embedded assets / 内嵌资源
themes/default/               # Default theme / 默认主题
web/templates/                # System templates / 系统模板 (admin, login, install)
```

---

## 🎨 Themes / 主题

```
themes/my-theme/
  theme.json       # Metadata / 元数据
  templates/       # HTML templates / 模板文件
```

Upload via admin panel → activate → no restart needed.

管理后台上传 → 激活 → 无需重启。

---

## 🔌 Plugins / 插件

WASM-based plugins can:

基于 WASM 的插件可以：

- Inject HTML/JS into pages / 向页面注入 HTML/JS
- Process post content / 处理文章内容
- Add challenge/verification flows / 添加验证流程

---

## 🛡️ Security / 安全

| | |
|---|---|
| Auth / 认证 | JWT in `HttpOnly`+`Secure`+`SameSite=Lax` cookie |
| Password / 密码 | bcrypt hashing / 哈希 |
| Rate Limit / 速率限制 | Login 5/15min, Comment 10/min, Search 30/min |
| Upload / 上传 | Extension whitelist, 10MB max / 扩展名白名单，最大 10MB |
| Comments / 评论 | HTML stripped, XSS protection / HTML 过滤，XSS 防护 |

---

## 🏗️ Build / 构建

```bash
# Current platform / 当前平台
go build -trimpath -ldflags "-s -w" -o polaris ./cmd/server

# Cross-compile / 交叉编译
CGO_ENABLED=0 GOOS=linux GOARCH=arm64 \
  go build -trimpath -ldflags "-s -w" -o polaris ./cmd/server
```

### GitHub Actions

The included workflow supports:

内置工作流支持：

- ✅ Cross-compile for Linux/macOS/Windows (amd64 + arm64)
- ✅ Optional GitHub Release with custom description
- ✅ Optional Docker multi-arch build (GHCR)

---

## 📄 License / 许可证

[MIT](LICENSE)
