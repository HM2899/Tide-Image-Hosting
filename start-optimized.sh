#!/bin/bash

echo "🎉 潮汐图床前端优化 - 测试部署"
echo "=================================="
echo ""

# 检查环境变量
if [ ! -f .env ]; then
    echo "⚠️  .env 文件不存在，从 .env.example 复制..."
    cp .env.example .env
    echo "✅ .env 文件已创建，请根据需要修改配置"
fi

# 检查前端构建
if [ ! -d "crates/web/dist" ]; then
    echo "❌ 前端未构建，请先运行："
    echo "   cd crates/web && trunk build"
    exit 1
fi

echo "✅ 前端已构建"
echo ""

# 检查后端构建
if [ ! -f "target/release/tide-server" ]; then
    echo "❌ 后端未构建，请先运行："
    echo "   cargo build -p tide-server --release"
    exit 1
fi

echo "✅ 后端已构建"
echo ""

echo "📋 优化内容："
echo "  ✅ 修复 OAuth hash 清理 bug"
echo "  ✅ 完整色彩系统（40+ 变量）"
echo "  ✅ 15+ 种流畅动画"
echo "  ✅ OAuth 按钮样式升级"
echo "  ✅ 性能优化（预加载、懒加载支持）"
echo "  ✅ 功能精简（代码减少 120 行）"
echo ""

echo "🚀 启动服务器..."
echo "   访问: http://localhost:18080"
echo "   按 Ctrl+C 停止"
echo ""

# 启动服务器
./target/release/tide-server
