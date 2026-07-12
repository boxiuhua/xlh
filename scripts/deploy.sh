#!/usr/bin/env bash
# 部署：把本机构建的镜像发到服务器并启动。
#
#   scripts/deploy.sh user@1.2.3.4                    # 部署到 /opt/xlh
#   scripts/deploy.sh user@1.2.3.4 /opt/xlh           # 自定义部署目录
#   WITH_PUSH=1 scripts/deploy.sh user@1.2.3.4        # 同时启动定时推送守护
#
# ## 数据安全的核心约定
#
# 部署目录（/opt/xlh）会被**覆盖**：镜像、compose、config 都是每次重发的。
# 状态目录（XLH_STATE_DIR，默认 = 部署目录 /opt/xlh）：数据在 /opt/xlh/data。
# 本脚本只 scp 具体文件进部署目录，**从不 rm -rf、从不整目录覆盖** —— 所以重新部署不会丢数据。
# 想彻底免疫手工误删，可把 XLH_STATE_DIR 指到部署目录之外（如 /srv/xlh-state）。
#
# 脚本会在部署前后各查一次 xlh.db 的用户数并比对 —— 数据没了会立刻报错，而不是等你发现。
set -euo pipefail

TARGET="${1:-}"
REMOTE_DIR="${2:-/opt/xlh}"
IMAGE="${XLH_IMAGE:-xlh:latest}"
STATE_DIR="${XLH_STATE_DIR:-$REMOTE_DIR}"   # 默认 = 部署目录 → 数据在 /opt/xlh/data
TARBALL="xlh-latest.tar.gz"

if [[ -z "$TARGET" ]]; then
  echo "用法: $0 user@host [部署目录]   （默认 /opt/xlh）" >&2
  echo "     状态目录由 XLH_STATE_DIR 决定（默认 = 部署目录，数据在 <部署目录>/data）" >&2
  exit 1
fi

case "$STATE_DIR" in
  /*) ;;
  *) echo "✗ XLH_STATE_DIR 必须是绝对路径，当前: $STATE_DIR" >&2; exit 1;;
esac

# 状态目录落在部署目录内部（如 /opt/xlh，数据即 /opt/xlh/data）。
#
# 本脚本**不会**删除部署目录 —— 它只 scp 几个具体文件（镜像包、compose、config）进去，
# 从不 rm -rf、从不 rsync --delete。所以这么放是可行的，也是默认配置。
#
# 但风险是真实的、且不在本脚本的控制范围内：任何一次手工的 `rm -rf /opt/xlh`、
# 整目录覆盖、或换用别的部署工具（rsync --delete / ansible copy），都会连数据一起抹掉。
# 想彻底免疫这类事故，把 XLH_STATE_DIR 指到部署目录之外（如 /srv/xlh-state）。
if [[ "$STATE_DIR" == "$REMOTE_DIR"/* || "$STATE_DIR" == "$REMOTE_DIR" ]]; then
  echo "ℹ 状态目录在部署目录内：$STATE_DIR/data"
  echo "  本脚本不会删除它（只 scp 具体文件，不做整目录覆盖）。"
  echo "  但要留意：手工 rm -rf $REMOTE_DIR 或用 rsync --delete 覆盖，会连数据一起没。"
  echo "  每次部署前会自动备份到 $STATE_DIR/backups/。"
  echo ""
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "==> 1/7 校验本机镜像 $IMAGE"
docker image inspect "$IMAGE" >/dev/null 2>&1 || {
  echo "✗ 本机没有 $IMAGE。先执行: docker build -t $IMAGE ." >&2
  exit 1
}
[[ -f docker-compose.prod.yml ]] || { echo "✗ 缺少 docker-compose.prod.yml" >&2; exit 1; }
[[ -f config.toml ]] || { echo "✗ 缺少 config.toml" >&2; exit 1; }

# 用 sqlite 数一下用户数；库不存在返回 -1（首次部署的正常情况）
count_users() {
  ssh "$TARGET" "
    db='$STATE_DIR/data/xlh.db'
    if [ ! -f \"\$db\" ]; then echo -1; exit 0; fi
    if command -v sqlite3 >/dev/null 2>&1; then
      sqlite3 \"\$db\" 'select count(*) from users;' 2>/dev/null || echo -2
    else
      # 服务器没装 sqlite3 就退而求其次：只要文件非空就认为有数据
      [ -s \"\$db\" ] && echo -3 || echo -1
    fi"
}

echo "==> 2/7 检查服务器现有数据（$STATE_DIR/data/xlh.db）"
BEFORE="$(count_users)"

# 老版本把数据放在部署目录里（$REMOTE_DIR/data）。新版挪到了 $STATE_DIR。
# 若不迁移，新库是空的 —— 服务照常起来，但**所有账号都登不上了**，看起来像"数据丢了"。
# 这个静默失败必须在这里堵死。
if [[ "$BEFORE" == "-1" ]]; then
  LEGACY="$(ssh "$TARGET" "[ -f '$REMOTE_DIR/data/xlh.db' ] && echo yes || echo no")"
  if [[ "$LEGACY" == "yes" ]]; then
    echo ""
    echo "  ⚠ 检测到旧版数据：$REMOTE_DIR/data/xlh.db"
    echo "    新版状态目录是 $STATE_DIR，里面还是空的。"
    echo "    不迁移的话，服务能起来但**所有账号都登不上**（新建了个空库）。"
    echo ""
    read -r -p "  现在把旧数据迁移到 $STATE_DIR/data/ ？(Y/n) " ans
    if [[ "${ans:-Y}" =~ ^[Yy]$|^$ ]]; then
      ssh "$TARGET" "
        set -e
        mkdir -p '$STATE_DIR/data'
        old='$REMOTE_DIR/data/xlh.db'
        new='$STATE_DIR/data/xlh.db'
        # 先停容器：库被占用时 .backup 可能读到不一致的状态
        (cd '$REMOTE_DIR' && docker compose -f docker-compose.prod.yml down 2>/dev/null) || true
        if command -v sqlite3 >/dev/null 2>&1; then
          # .backup 会把 WAL checkpoint 进去。直接 cp 主库会拷到空壳 —— 库在 WAL 模式下，
          # 数据可能几乎全在 -wal 里（实测：主库 4KB、WAL 78KB）。
          sqlite3 \"\$old\" \".backup '\$new'\"
        else
          cp \"\$old\" \"\$new\"
          [ -f \"\$old-wal\" ] && cp \"\$old-wal\" \"\$new-wal\"
          [ -f \"\$old-shm\" ] && cp \"\$old-shm\" \"\$new-shm\"
        fi
        # 缓存可再生，但重抓很慢，一并搬过去
        if [ -d '$REMOTE_DIR/.cache' ]; then
          mkdir -p '$STATE_DIR/cache'
          cp -r '$REMOTE_DIR/.cache/.' '$STATE_DIR/cache/' 2>/dev/null || true
        fi
        # 旧目录改名而非删除 —— 迁移出错时还有回头路
        mv '$REMOTE_DIR/data' '$REMOTE_DIR/data.migrated-\$(date +%Y%m%d-%H%M%S)' 2>/dev/null || true
        echo '    ✓ 迁移完成（旧目录已改名保留，确认无误后可自行删除）'"
      BEFORE="$(count_users)"
      echo "    迁移后用户数：$BEFORE"
    else
      echo "  ✗ 已取消。请手工迁移后再部署，否则账号会全部登不上。" >&2
      exit 1
    fi
  fi
fi

case "$BEFORE" in
  -1) echo "    首次部署：尚无数据库";;
  -2) echo "    ⚠ 数据库存在但读取失败（可能损坏）";;
  -3) echo "    数据库存在（服务器无 sqlite3，无法数用户数）";;
  *)  echo "    现有 $BEFORE 个用户 —— 部署后会再查一次并比对";;
esac

# 有数据就先备份。备份是廉价的，丢数据是不可逆的。
if [[ "$BEFORE" != "-1" ]]; then
  echo "==> 3/7 部署前备份数据库"
  ssh "$TARGET" "
    set -e
    mkdir -p '$STATE_DIR/backups'
    ts=\$(date +%Y%m%d-%H%M%S)
    out=\"$STATE_DIR/backups/xlh-\$ts.db\"
    db='$STATE_DIR/data/xlh.db'
    if command -v sqlite3 >/dev/null 2>&1; then
      # .backup 会把 WAL checkpoint 进去；cp 主库则会漏掉 WAL 里的数据
      sqlite3 \"\$db\" \".backup '\$out'\"
    else
      # 库跑在 WAL 模式：只 cp 主库会得到一个 4KB 空壳（能打开、看着正常、其实没数据）。
      # 必须连 -wal/-shm 一起拷。
      cp \"\$db\" \"\$out\"
      [ -f \"\$db-wal\" ] && cp \"\$db-wal\" \"\$out-wal\"
      [ -f \"\$db-shm\" ] && cp \"\$db-shm\" \"\$out-shm\"
    fi
    echo \"    已备份到 \$out\"
    # 只留最近 10 份（含其 -wal/-shm）
    ls -1t '$STATE_DIR/backups'/xlh-*.db 2>/dev/null | tail -n +11 | while read -r old; do
      rm -f \"\$old\" \"\$old-wal\" \"\$old-shm\"
    done"
else
  echo "==> 3/7 无既有数据，跳过备份"
fi

echo "==> 4/7 导出镜像"
docker save "$IMAGE" | gzip > "$TARBALL"
echo "    $(du -h "$TARBALL" | cut -f1)"

echo "==> 5/7 准备服务器目录"
ssh "$TARGET" "mkdir -p '$REMOTE_DIR' '$STATE_DIR/data' '$STATE_DIR/cache' '$STATE_DIR/output' '$STATE_DIR/backups'"

echo "==> 6/7 上传并启动"
scp -q "$TARBALL" docker-compose.prod.yml config.toml "$TARGET:$REMOTE_DIR/"

# .env 只在服务器上不存在时才生成 —— 已有的不覆盖（里面可能有手改过的端口/时区）
ssh "$TARGET" "
  set -e
  cd '$REMOTE_DIR'
  if [ ! -f .env ]; then
    cat > .env <<EOF
XLH_STATE_DIR=$STATE_DIR
XLH_IMAGE=$IMAGE
XLH_BIND_ADDR=127.0.0.1
XLH_PORT=8080
TZ=Asia/Shanghai
EOF
    echo '    已生成 .env'
  else
    echo '    .env 已存在，保持不动'
  fi"

# xlh-push 在 compose 里是可选 profile。不带 --profile push 时，`docker compose up -d`
# **完全不管这个服务** —— 已存在的 xlh-push 容器不会被重建、不会被停、连看都不看一眼。
#
# 真实踩过的坑：服务器上的 xlh-push 是用老 compose 建的（command 还是 `push --file push.toml`，
# 而 --file 早已被移除），于是 clap 参数解析失败、退出码 2、无限崩溃重启。每次部署它都被
# 跳过，永远修不好 —— 而用户只看到「推送不生效」，还以为是 cron 的问题。
#
# 所以：只要服务器上**已经有** xlh-push 容器，就一律带上 profile 去接管它，不管 WITH_PUSH。
PROFILE_ARGS=""
if [[ "${WITH_PUSH:-0}" == "1" ]]; then
  PROFILE_ARGS="--profile push"
elif ssh "$TARGET" "docker ps -a --filter name=xlh-push --format '{{.Names}}' | grep -q xlh-push"; then
  PROFILE_ARGS="--profile push"
  echo "    检测到已有 xlh-push 容器 → 一并重建（否则它会带着旧命令烂在那儿）"
fi

ssh "$TARGET" "set -e; cd '$REMOTE_DIR'; \
  gunzip -c '$TARBALL' | docker load; \
  docker compose -f docker-compose.prod.yml $PROFILE_ARGS up -d --force-recreate --remove-orphans; \
  rm -f '$TARBALL'; \
  docker compose -f docker-compose.prod.yml $PROFILE_ARGS ps"

echo "==> 7/7 校验数据仍在 + 容器真的健康"
sleep 8   # 崩溃循环的容器需要几秒才会显露出 Restarting 状态
rm -f "$TARBALL"

# 「起来了」不等于「活着」。崩溃循环的容器在 `ps` 里也有一行，只是状态是 Restarting ——
# 这次就是这么被忽略了整整 4 小时。所以要显式检查。
UNHEALTHY="$(ssh "$TARGET" "docker ps -a --filter name=xlh- --format '{{.Names}} {{.Status}}' \
  | grep -Ei 'restarting|exited' || true")"
if [[ -n "$UNHEALTHY" ]]; then
  echo "" >&2
  echo "✗ 有容器处于崩溃/退出状态：" >&2
  echo "$UNHEALTHY" | sed 's/^/    /' >&2
  echo "" >&2
  echo "  看日志定位：" >&2
  echo "  ssh $TARGET 'docker logs --tail 30 \$(echo \"$UNHEALTHY\" | head -1 | cut -d\" \" -f1)'" >&2
  exit 1
fi
echo "    ✓ 所有容器运行正常"

AFTER="$(count_users)"

if [[ "$BEFORE" =~ ^[0-9]+$ ]]; then
  if [[ "$AFTER" =~ ^[0-9]+$ ]] && [[ "$AFTER" -ge "$BEFORE" ]]; then
    echo "    ✓ 用户数 $BEFORE → $AFTER，数据完好"
  else
    echo "" >&2
    echo "✗✗ 数据可能丢失！部署前 $BEFORE 个用户，部署后 $AFTER。" >&2
    echo "    备份在 $STATE_DIR/backups/，立即检查：" >&2
    echo "    ssh $TARGET 'ls -lt $STATE_DIR/backups/'" >&2
    exit 1
  fi
else
  echo "    （首次部署，无可比对的基线）"
fi

echo ""
echo "✅ 部署完成"
echo "   数据    ：$STATE_DIR/data/xlh.db"
if [[ "$STATE_DIR" == "$REMOTE_DIR" || "$STATE_DIR" == "$REMOTE_DIR"/* ]]; then
  echo "             （在部署目录内。本脚本不会删它，但手工 rm -rf $REMOTE_DIR 会连它一起没）"
else
  echo "             （在部署目录之外，任何部署动作都碰不到）"
fi
echo "   备份    ：$STATE_DIR/backups/（每次部署前自动备份，保留最近 10 份）"
echo "   日志    ：ssh $TARGET 'cd $REMOTE_DIR && docker compose -f docker-compose.prod.yml logs -f'"
echo "   Web 绑在 127.0.0.1:8080，请在其前面配 Nginx/Caddy 反代 + HTTPS 后再对外访问。"
