FROM alpine:latest

WORKDIR /app

# 安装必要的依赖
RUN apk add --no-cache ca-certificates tzdata

# 设置时区
ENV TZ=Asia/Shanghai

COPY app /app/app
RUN chmod +x /app/app

EXPOSE 8080
ENTRYPOINT ["/app/app"]
