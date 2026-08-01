# 打包与 Docker 部署

本文说明如何生成 Windows 可执行包和 Linux amd64 Docker 镜像包，以及如何在服务器部署。

## Windows 包

在项目根目录执行：

```powershell
cargo build --release --bin xlh

$version = Get-Date -Format 'yyyyMMdd'
$dir = "output/xlh-windows-x86_64-release-$version"
$zip = "xlh-windows-x86_64-release-$version.zip"
New-Item -ItemType Directory -Path $dir
Copy-Item target/release/xlh.exe,.env.example,config.toml,DEPLOY.md,docker-compose.prod.yml,README.md -Destination $dir
Compress-Archive -LiteralPath (Get-ChildItem $dir -File | ForEach-Object FullName) -DestinationPath $zip -CompressionLevel Optimal
(Get-FileHash $zip -Algorithm SHA256).Hash.ToLower() + "  $zip" | Set-Content "$zip.sha256" -NoNewline
```

包内包含 `xlh.exe`、默认配置、环境变量示例及部署文档。

## Docker 镜像包

要求：Docker Desktop 或 Linux Docker Engine 已启动。

```powershell
$version = Get-Date -Format 'yyyyMMdd'
$tar = "xlh-latest-$version.tar"
$gzip = "xlh-latest-$version.tar.gz"

docker build --provenance=false --sbom=false -t xlh:latest .
docker save xlh:latest -o $tar

# 直接 gzip 压缩镜像 tar；不要再包一层 tar。
$input = [System.IO.File]::OpenRead((Resolve-Path $tar))
try {
  $output = [System.IO.File]::Create((Join-Path (Get-Location) $gzip))
  try {
    $stream = [System.IO.Compression.GZipStream]::new($output, [System.IO.Compression.CompressionLevel]::Optimal)
    try { $input.CopyTo($stream) } finally { $stream.Dispose() }
  } finally { $output.Dispose() }
} finally { $input.Dispose() }

(Get-FileHash $gzip -Algorithm SHA256).Hash.ToLower() + "  $gzip" | Set-Content "$gzip.sha256" -NoNewline
docker load -i $gzip
```

最后一条 `docker load` 是本地可读性验证；成功时应显示 `Loaded image: xlh:latest`。

## 上传与服务器部署

将镜像包、生产 Compose 文件和配置上传到服务器：

```bash
scp xlh-latest-YYYYMMDD.tar.gz xlh-latest-YYYYMMDD.tar.gz.sha256 \
  docker-compose.prod.yml config.toml user@SERVER:/opt/xlh/
```

服务器执行：

```bash
cd /opt/xlh
sha256sum -c xlh-latest-YYYYMMDD.tar.gz.sha256

cat > .env <<'EOF'
XLH_STATE_DIR=/opt/xlh
XLH_IMAGE=xlh:latest
XLH_BIND_ADDR=127.0.0.1
XLH_PORT=8080
TZ=Asia/Shanghai
EOF

mkdir -p /opt/xlh/{data,cache,output}
docker load -i xlh-latest-YYYYMMDD.tar.gz
docker compose -f docker-compose.prod.yml --profile push up -d --force-recreate --remove-orphans
docker compose -f docker-compose.prod.yml --profile push ps
```

`xlh-push` 位于 `push` profile。若需要盘中异动监控和消息推送，启动命令必须带 `--profile push`。

## 更新

更新时保留 `/opt/xlh/data`、`/opt/xlh/cache` 和 `/opt/xlh/output`，只上传新镜像包与配置后执行：

```bash
cd /opt/xlh
docker load -i xlh-latest-NEW.tar.gz
docker compose -f docker-compose.prod.yml --profile push up -d --force-recreate
docker logs --tail 50 xlh-web
docker logs --tail 50 xlh-push
```

不要删除 `XLH_STATE_DIR`，其中保存用户、授权、推送配置和历史数据。
