#!/usr/bin/env bash
# Baton 一键启动：构建 WebUI + 编译 core 全部二进制 + 启动 HTTP server（内嵌 serve WebUI）
# 用法：./start.sh 然后打开 http://127.0.0.1:7700
# 说明：baton-mcp 是 stdio 进程，由 Agent 客户端（如 Claude Code）按需 spawn，
#       无需常驻；这里编译它只是保证二进制存在，便于 `claude mcp add` 注册。
set -euo pipefail
cd "$(dirname "$0")"

# 首次运行：安装前端依赖
if [ ! -d web/node_modules ]; then
  echo "==> 安装前端依赖..."
  (cd web && npm install)
fi

# 前端：dist 缺失或源码有更新时重建
if [ ! -f web/dist/index.html ] || [ -n "$(find web/src web/index.html -newer web/dist/index.html -print -quit 2>/dev/null)" ]; then
  echo "==> 构建 WebUI (web/dist)..."
  (cd web && npm run build)
fi

echo "==> 编译 core（baton-core / baton-mcp / baton）..."
(cd core && cargo build)

echo "==> 启动 Baton → http://127.0.0.1:7700"
exec core/target/debug/baton-core
