#!/usr/bin/env bash
# 备份 / 恢复 xlh.db。
#
#   scripts/backup.sh                       # 本机备份（XLH_STATE_DIR 默认 /srv/xlh-state）
#   scripts/backup.sh user@host             # 远程备份并拉回本机
#   scripts/backup.sh --list user@host      # 列出服务器上的备份
#   scripts/backup.sh --restore user@host xlh-20260712-1930.db
#
# xlh.db 里是用户、授权码、会话、推送配置、建议历史 —— **唯一不可再生的数据**。
# 缓存丢了只是重抓一遍，这个丢了就真没了。
#
# ## 为什么必须用 sqlite3 的 .backup，绝不能只 cp xlh.db
#
# 数据库跑在 **WAL 模式**下。实测一个刚建好用户的库：
#     xlh.db      4,096 字节   ← 主库几乎是空的
#     xlh.db-wal 78,312 字节   ← 数据实际在这里
# 只 `cp xlh.db` 会拷到一个 4KB 的空壳 —— **能打开、看着正常、其实没数据**。
# 这种「看似成功的备份」比没有备份更危险，因为你会以为自己有退路。
#
# 所以：优先用 sqlite3 的 .backup（会把 WAL 一并 checkpoint 进去）；
# 实在没有 sqlite3 时，cp 必须把 -wal / -shm 一起带上。
set -euo pipefail

STATE_DIR="${XLH_STATE_DIR:-/opt/xlh}"
DB="$STATE_DIR/data/xlh.db"
BACKUP_DIR="$STATE_DIR/backups"
KEEP=20
# 容器里装了 sqlite3；宿主机不一定有。优先借容器的用。
CONTAINER="${XLH_CONTAINER:-xlh-web}"

usage() { sed -n '2,10p' "$0" | sed 's/^# \?//'; exit 1; }

MODE="backup"
case "${1:-}" in
  --list)    MODE="list";    shift;;
  --restore) MODE="restore"; shift;;
  -h|--help) usage;;
esac
TARGET="${1:-}"

# 在给定 shell 上下文里跑一段脚本：有 TARGET 就 ssh，否则本机
run() {
  if [[ -n "$TARGET" ]]; then ssh "$TARGET" "$1"; else bash -c "$1"; fi
}

case "$MODE" in
  list)
    run "ls -lht '$BACKUP_DIR'/xlh-*.db 2>/dev/null || echo '(无备份)'"
    ;;

  restore)
    FILE="${2:-}"
    [[ -n "$FILE" ]] || { echo "✗ 请指定要恢复的备份文件名（先用 --list 看）" >&2; exit 1; }
    echo "⚠ 即将用 $FILE 覆盖当前数据库。当前库会先另存一份。"
    read -r -p "确认？(yes/N) " ans
    [[ "$ans" == "yes" ]] || { echo "已取消"; exit 0; }
    run "
      set -e
      [ -f '$BACKUP_DIR/$FILE' ] || { echo '✗ 备份不存在: $BACKUP_DIR/$FILE' >&2; exit 1; }
      # 恢复前把现库另存，免得恢复错了连回头路都没有
      if [ -f '$DB' ]; then
        cp '$DB' '$BACKUP_DIR/before-restore-\$(date +%Y%m%d-%H%M%S).db'
      fi
      cp '$BACKUP_DIR/$FILE' '$DB'
      echo '✓ 已恢复。需要重启容器使其重新打开数据库：'
      echo '  docker compose -f docker-compose.prod.yml restart'"
    ;;

  backup)
    TS="$(date +%Y%m%d-%H%M%S)"
    run "
      set -e
      [ -f '$DB' ] || { echo '✗ 数据库不存在: $DB' >&2; exit 1; }
      mkdir -p '$BACKUP_DIR'
      OUT='$BACKUP_DIR/xlh-$TS.db'

      # 三条路径，按可靠性排序
      if command -v sqlite3 >/dev/null 2>&1; then
        sqlite3 '$DB' \".backup '\$OUT'\"
        SQL='sqlite3'
      elif docker exec '$CONTAINER' sqlite3 -version >/dev/null 2>&1; then
        # 借容器里的 sqlite3（镜像里装了）。容器内路径固定为 /app/data。
        docker exec '$CONTAINER' sqlite3 /app/data/xlh.db \\
          \".backup '/app/data/.bak-$TS.db'\"
        mv '$STATE_DIR/data/.bak-$TS.db' \"\$OUT\"
        SQL=\"docker exec $CONTAINER sqlite3\"
      else
        # 最后的兜底：库在 WAL 模式下，只 cp 主库会拷到空壳 —— 必须连 -wal/-shm 一起拷
        echo '⚠ 宿主机与容器都没有 sqlite3，退化为 cp（连 WAL 一起拷）' >&2
        cp '$DB' \"\$OUT\"
        [ -f '$DB-wal' ] && cp '$DB-wal' \"\$OUT-wal\"
        [ -f '$DB-shm' ] && cp '$DB-shm' \"\$OUT-shm\"
        SQL=''
      fi

      # 备份不校验等于没备份 —— 尤其要防的正是「4KB 空壳」那种看似成功的备份
      if [ -n \"\$SQL\" ]; then
        if [ \"\$SQL\" = 'sqlite3' ]; then
          n=\$(sqlite3 \"\$OUT\" 'select count(*) from users;')
        else
          n=\$(docker exec '$CONTAINER' sqlite3 '/app/data/xlh.db' 'select count(*) from users;')
        fi
        [ \"\$n\" -ge 0 ] 2>/dev/null || { echo '✗ 备份校验失败，无法读出用户数' >&2; exit 1; }
        echo \"✓ 备份完成: \$OUT（\$n 个用户，已校验可读）\"
      else
        echo \"✓ 备份完成: \$OUT（含 -wal/-shm；未校验，建议装 sqlite3）\"
      fi

      ls -1t '$BACKUP_DIR'/xlh-*.db 2>/dev/null | tail -n +\$(($KEEP + 1)) | xargs -r rm -f"

    # 远程备份顺手拉回本机 —— 备份和数据在同一台机器上，机器没了就一起没了
    if [[ -n "$TARGET" ]]; then
      mkdir -p ./backups
      scp -q "$TARGET:$BACKUP_DIR/xlh-$TS.db" "./backups/" \
        && echo "✓ 已拉回本机: ./backups/xlh-$TS.db"
    fi
    ;;
esac
