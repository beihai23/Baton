#!/usr/bin/env bash
# Baton 一键启动：构建 WebUI + 编译 core 全部二进制 + 启动 HTTP server（内嵌 serve WebUI）
# 用法：./start.sh 然后打开 http://127.0.0.1:7700
# 说明：baton-mcp 是 stdio 进程，由 Agent 客户端（如 Claude Code）按需 spawn，
#       无需常驻；这里编译它只是保证二进制存在，便于 `claude mcp add` 注册。
# 用法：./start.sh        启动（编译缓存会保留，重启秒开）
#       ./start.sh clean  清理编译中间产物（core/target、web/node_modules、web/dist），
#                         下次启动需重新装依赖并全量编译（较慢）
set -euo pipefail
cd "$(dirname "$0")"

if [ "${1:-}" = "clean" ]; then
  echo "==> 清理编译中间产物..."
  rm -rf core/target web/node_modules web/dist
  echo "完成（src-tauri/target 属桌面应用构建，如需清理请单独执行 rm -rf src-tauri/target）。"
  exit 0
fi

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
