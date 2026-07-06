# syntax=docker/dockerfile:1

########## 构建阶段 ##########
FROM rust:1-bookworm AS builder

# plotters -> font-kit/freetype-sys 需要原生库；reqwest 用 rustls，无需 openssl
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config \
        libfreetype6-dev \
        libfontconfig1-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# 先复制清单，利用 Docker 层缓存单独缓存依赖编译
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs \
    && echo '' > src/lib.rs \
    && cargo build --release --bin xlh 2>/dev/null || true
RUN rm -rf src

# 复制真实源码并构建
COPY src ./src
COPY tests ./tests
# 触碰以确保重新编译（占位构建可能缓存了空壳）
RUN touch src/main.rs src/lib.rs \
    && cargo build --release --bin xlh

########## 运行阶段 ##########
FROM debian:bookworm-slim AS runtime

# 运行期依赖：freetype/fontconfig + 字体（plotters 画 PNG 用），ca-certificates 兜底，
# tzdata 提供时区库（否则 chrono::Local 回落 UTC，定时推送 cron 会差 8 小时）。
RUN apt-get update && apt-get install -y --no-install-recommends \
        libfreetype6 \
        libfontconfig1 \
        fonts-dejavu-core \
        ca-certificates \
        tzdata \
    && rm -rf /var/lib/apt/lists/*

# 默认时区设为北京时间：定时推送的 cron 按本地时间解释，与用户预期一致（可用 -e TZ=... 覆盖）。
ENV TZ=Asia/Shanghai
RUN ln -snf /usr/share/zoneinfo/$TZ /etc/localtime && echo "$TZ" > /etc/timezone

WORKDIR /app

COPY --from=builder /app/target/release/xlh /usr/local/bin/xlh

# 数据/缓存/输出目录（可挂载卷持久化）
RUN mkdir -p /app/.cache /app/output

# 容器内对外暴露 Web 界面需要监听 0.0.0.0
ENV XLH_BIND=0.0.0.0
EXPOSE 8080

# 默认启动 Web 界面；可用 `docker run ... xlh push --once` 等覆盖
ENTRYPOINT ["xlh"]
CMD ["serve", "--port", "8080"]
