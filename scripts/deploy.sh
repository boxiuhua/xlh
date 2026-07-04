#!/usr/bin/env bash
# 一键部署：把本机 xlh:latest 镜像 + 配置发到线上服务器并启动。
# 用法：
#   scripts/deploy.sh user@1.2.3.4                 # 部署到 /opt/xlh，仅 Web
#   scripts/deploy.sh user@1.2.3.4 /srv/xlh        # 自定义远端目录
#   WITH_PUSH=1 scripts/deploy.sh user@1.2.3.4      # 同时启动定时推送守护
#
# 前提：本机已 docker build 出 xlh:latest；服务器已装 docker + docker compose；
#       本机能免密或交互式 ssh 到服务器（脚本会多次 ssh/scp）。
set -euo pipefail

TARGET="${1:-}"
REMOTE_DIR="${2:-/opt/xlh}"
IMAGE="xlh:latest"
TARBALL="xlh-latest.tar.gz"

if [[ -z "$TARGET" ]]; then
  echo "用法: $0 user@host [远端目录]   （默认 /opt/xlh）" >&2
  exit 1
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "==> 1/5 校验本机镜像 $IMAGE"
docker image inspect "$IMAGE" >/dev/null 2>&1 || {
  echo "本机没有 $IMAGE，请先在项目根目录执行 docker build -t $IMAGE ." >&2
  exit 1
}

for f in docker-compose.prod.yml config.toml push.toml; do
  [[ -f "$f" ]] || { echo "缺少 $f，无法部署" >&2; exit 1; }
done

echo "==> 2/5 导出镜像为 $TARBALL"
docker save "$IMAGE" | gzip > "$TARBALL"

echo "==> 3/5 在服务器创建目录 $REMOTE_DIR"
ssh "$TARGET" "mkdir -p '$REMOTE_DIR' '$REMOTE_DIR/.cache' '$REMOTE_DIR/output'"

echo "==> 4/5 上传镜像与配置到 $TARGET:$REMOTE_DIR"
scp "$TARBALL" docker-compose.prod.yml config.toml push.toml "$TARGET:$REMOTE_DIR/"

echo "==> 5/5 服务器导入镜像并启动"
PROFILE_ARGS=""
[[ "${WITH_PUSH:-0}" == "1" ]] && PROFILE_ARGS="--profile push"
ssh "$TARGET" "set -e; cd '$REMOTE_DIR'; \
  gunzip -c '$TARBALL' | docker load; \
  docker compose -f docker-compose.prod.yml $PROFILE_ARGS up -d; \
  docker compose -f docker-compose.prod.yml ps"

rm -f "$TARBALL"
echo ""
echo "✅ 部署完成。查看日志：ssh $TARGET 'cd $REMOTE_DIR && docker compose -f docker-compose.prod.yml logs -f'"
echo "   Web 默认绑在服务器的 127.0.0.1:8080，请在其前面配置 Nginx/Caddy 反代 + HTTPS + 认证后再对外访问。"
