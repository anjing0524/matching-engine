# 部署指南

**项目**: Rust高性能期货撮合引擎
**版本**: v0.1.0
**更新日期**: 2025-11-18

---

## 📋 目录

1. [环境要求](#环境要求)
2. [构建部署](#构建部署)
3. [配置管理](#配置管理)
4. [运行模式](#运行模式)
5. [监控告警](#监控告警)
6. [故障排查](#故障排查)
7. [性能优化](#性能优化)
8. [安全加固](#安全加固)

---

## 🔧 环境要求

### 硬件要求

**最小配置** (开发/测试):
- CPU: 4核 @ 2.0GHz
- 内存: 8GB RAM
- 磁盘: 20GB SSD
- 网络: 1Gbps

**推荐配置** (生产环境):
- CPU: 16核+ @ 3.0GHz (Intel Xeon/AMD EPYC)
- 内存: 64GB+ RAM
- 磁盘: 500GB+ NVMe SSD (IOPS 10K+)
- 网络: 10Gbps+ 低延迟网卡
- 可选: NUMA架构支持

**高性能配置** (HFT场景):
- CPU: 32核+ @ 4.0GHz (支持AVX2/AVX512)
- 内存: 128GB+ DDR5 RAM
- 磁盘: 企业级NVMe (延迟<100μs)
- 网络: 25Gbps+ RDMA网卡或DPDK支持
- 可选: FPGA加速卡

### 软件要求

**操作系统**:
- Linux: Ubuntu 22.04 LTS / CentOS 8+ / RHEL 8+
- 内核: 5.10+ (支持io_uring需要5.1+)

**运行时依赖**:
- glibc 2.31+
- libm, libpthread (通常已安装)

**可选依赖**:
- DPDK 21.11+ (启用dpdk特性)
- FPGA驱动 (启用fpga特性)

---

## 🏗️ 构建部署

### 方式1: 标准Release构建

**适用场景**: 生产环境，平衡性能和二进制大小

```bash
# 1. 克隆代码
git clone https://github.com/your-org/matching-engine.git
cd matching-engine

# 2. 安装Rust (如未安装)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

# 3. 构建release版本
cargo build --release

# 4. 验证构建
./target/release/matching-engine --version

# 5. 运行测试
cargo test --release

# 6. 安装到系统
sudo cp target/release/matching-engine /usr/local/bin/
```

**构建产物**:
- 二进制: `target/release/matching-engine` (~11MB)
- 编译时间: ~1.5分钟 (16核)

### 方式2: 体积优化构建

**适用场景**: 容器化部署，最小化镜像大小

```bash
# 使用release-small profile
cargo build --profile release-small

# 产物大小: ~5-6MB (相比release减少50%)
```

### 方式3: 特性定制构建

**启用高性能特性**:

```bash
# CPU亲和性 (绑定核心)
cargo build --release --features cpu-affinity

# io_uring网络 (Linux 5.1+)
cargo build --release --features io-uring

# 组合特性
cargo build --release --features "cpu-affinity,io-uring"
```

### 方式4: Docker容器化部署

**Dockerfile**:

```dockerfile
# 多阶段构建
FROM rust:1.75 as builder

WORKDIR /app
COPY . .

# 使用release-small优化体积
RUN cargo build --profile release-small

# 最小运行时镜像
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release-small/matching-engine /usr/local/bin/

# 创建非root用户
RUN useradd -m -u 1000 appuser
USER appuser

EXPOSE 8080
EXPOSE 9090

CMD ["matching-engine", "--config", "/etc/matching-engine/config.toml"]
```

**构建并运行**:

```bash
# 构建镜像
docker build -t matching-engine:latest .

# 运行容器
docker run -d \
  --name matching-engine \
  -p 8080:8080 \
  -p 9090:9090 \
  -v /etc/matching-engine:/etc/matching-engine:ro \
  -v /var/log/matching-engine:/var/log:rw \
  matching-engine:latest
```

---

## ⚙️ 配置管理

### 配置文件示例

**config.toml**:

```toml
[engine]
# 引擎类型: "tick-based" (推荐) | "legacy"
engine_type = "tick-based"

# 合约规格
[contract]
symbol = "BTC/USD"
tick_size = 1           # 最小价格变动单位
min_price = 1000        # 最低价格
max_price = 100000      # 最高价格

[partitioning]
# 分区数量 (建议=CPU核心数)
partition_count = 16
# 每分区队列容量
queue_capacity = 10000
# 批量处理大小
batch_size = 100
# CPU亲和性绑定
enable_cpu_affinity = true

[network]
# 网络后端: "tokio" | "io-uring" | "dpdk"
backend = "io-uring"
listen_addr = "0.0.0.0:8080"
buffer_size = 8192
queue_depth = 1024

[persistence]
# 启用持久化
enabled = true
# WAL路径
wal_path = "/var/lib/matching-engine/wal"
# 快照路径
snapshot_path = "/var/lib/matching-engine/snapshots"
# 自动刷盘间隔(ms)
auto_flush_interval = 1000

[observability]
# Prometheus指标端口
metrics_port = 9090
# 健康检查端口
health_port = 8081
# 日志级别: "trace" | "debug" | "info" | "warn" | "error"
log_level = "info"
# 日志格式: "json" | "text"
log_format = "json"
```

### 环境变量

```bash
# 日志级别
export RUST_LOG=info

# 回溯信息
export RUST_BACKTRACE=1

# 内存分配器 (jemalloc)
export MALLOC_CONF="background_thread:true,metadata_thp:auto,dirty_decay_ms:10000"

# CPU亲和性
export MATCHING_ENGINE_CPU_AFFINITY=true

# 配置文件路径
export MATCHING_ENGINE_CONFIG=/etc/matching-engine/config.toml
```

### 命令行参数

```bash
matching-engine \
  --config /etc/matching-engine/config.toml \
  --log-level info \
  --metrics-port 9090 \
  --listen 0.0.0.0:8080
```

---

## 🚀 运行模式

### 1. 开发模式

```bash
# 使用默认配置
cargo run

# 带日志
RUST_LOG=debug cargo run
```

### 2. 生产模式 - systemd服务

**matching-engine.service**:

```ini
[Unit]
Description=Rust Matching Engine
After=network.target

[Service]
Type=simple
User=matching-engine
Group=matching-engine
WorkingDirectory=/opt/matching-engine

# 环境变量
Environment=RUST_LOG=info
Environment=RUST_BACKTRACE=1
Environment=MALLOC_CONF=background_thread:true

# 启动命令
ExecStart=/usr/local/bin/matching-engine \
  --config /etc/matching-engine/config.toml

# 重启策略
Restart=always
RestartSec=10

# 资源限制
LimitNOFILE=65536
LimitNPROC=4096

# 安全加固
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/matching-engine /var/log/matching-engine

[Install]
WantedBy=multi-user.target
```

**部署步骤**:

```bash
# 1. 创建用户
sudo useradd -r -s /bin/false matching-engine

# 2. 创建目录
sudo mkdir -p /etc/matching-engine
sudo mkdir -p /var/lib/matching-engine/{wal,snapshots}
sudo mkdir -p /var/log/matching-engine
sudo chown -R matching-engine:matching-engine /var/lib/matching-engine
sudo chown -R matching-engine:matching-engine /var/log/matching-engine

# 3. 安装服务
sudo cp matching-engine.service /etc/systemd/system/
sudo systemctl daemon-reload

# 4. 启动服务
sudo systemctl enable matching-engine
sudo systemctl start matching-engine

# 5. 检查状态
sudo systemctl status matching-engine
sudo journalctl -u matching-engine -f
```

### 3. Kubernetes部署

**deployment.yaml**:

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: matching-engine
  namespace: trading
spec:
  replicas: 3
  selector:
    matchLabels:
      app: matching-engine
  template:
    metadata:
      labels:
        app: matching-engine
    spec:
      containers:
      - name: matching-engine
        image: matching-engine:latest
        ports:
        - containerPort: 8080
          name: service
        - containerPort: 9090
          name: metrics
        resources:
          requests:
            cpu: "4"
            memory: "8Gi"
          limits:
            cpu: "16"
            memory: "32Gi"
        env:
        - name: RUST_LOG
          value: "info"
        volumeMounts:
        - name: config
          mountPath: /etc/matching-engine
          readOnly: true
        - name: data
          mountPath: /var/lib/matching-engine
        livenessProbe:
          httpGet:
            path: /health/live
            port: 8081
          initialDelaySeconds: 30
          periodSeconds: 10
        readinessProbe:
          httpGet:
            path: /health/ready
            port: 8081
          initialDelaySeconds: 5
          periodSeconds: 5
      volumes:
      - name: config
        configMap:
          name: matching-engine-config
      - name: data
        persistentVolumeClaim:
          claimName: matching-engine-pvc
---
apiVersion: v1
kind: Service
metadata:
  name: matching-engine
  namespace: trading
spec:
  selector:
    app: matching-engine
  ports:
  - name: service
    port: 8080
    targetPort: 8080
  - name: metrics
    port: 9090
    targetPort: 9090
  type: ClusterIP
```

---

## 📊 监控告警

### Prometheus指标

**暴露的指标**:

```
# 订单指标
matching_engine_orders_total{type="buy|sell", symbol="*"}
matching_engine_order_latency_seconds{symbol="*"}

# 成交指标
matching_engine_trades_total{symbol="*"}
matching_engine_trade_volume{symbol="*"}

# 性能指标
matching_engine_throughput_ops_per_sec
matching_engine_queue_depth{partition="N"}

# 系统指标
matching_engine_memory_usage_bytes
matching_engine_cpu_usage_percent
```

**Prometheus配置** (prometheus.yml):

```yaml
scrape_configs:
  - job_name: 'matching-engine'
    static_configs:
      - targets: ['localhost:9090']
    scrape_interval: 5s
```

### Grafana仪表盘

**关键面板**:

1. **吞吐量**: matching_engine_throughput_ops_per_sec
2. **延迟分布**: histogram_quantile(0.99, matching_engine_order_latency_seconds)
3. **订单队列**: matching_engine_queue_depth
4. **成交量**: rate(matching_engine_trades_total[1m])

**示例PromQL**:

```promql
# P99延迟
histogram_quantile(0.99,
  rate(matching_engine_order_latency_seconds_bucket[5m])
)

# 每秒吞吐量
rate(matching_engine_orders_total[1m])

# 成交率
rate(matching_engine_trades_total[1m]) /
rate(matching_engine_orders_total[1m])
```

### 告警规则

**alerts.yml**:

```yaml
groups:
  - name: matching-engine
    rules:
      # 高延迟告警
      - alert: HighLatency
        expr: histogram_quantile(0.99, matching_engine_order_latency_seconds) > 0.001
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "P99延迟超过1ms"

      # 低吞吐告警
      - alert: LowThroughput
        expr: rate(matching_engine_orders_total[5m]) < 100000
        for: 10m
        labels:
          severity: warning
        annotations:
          summary: "吞吐量低于100K ops/s"

      # 服务不健康
      - alert: ServiceUnhealthy
        expr: up{job="matching-engine"} == 0
        for: 1m
        labels:
          severity: critical
        annotations:
          summary: "撮合引擎服务不可用"

      # 内存泄漏检测
      - alert: MemoryLeak
        expr: rate(matching_engine_memory_usage_bytes[1h]) > 1048576
        for: 30m
        labels:
          severity: warning
        annotations:
          summary: "检测到可能的内存泄漏"
```

---

## 🔍 故障排查

### 常见问题

#### 1. 性能下降

**症状**: 吞吐量降低，延迟增加

**排查步骤**:

```bash
# 1. 检查CPU使用率
top -H -p $(pgrep matching-engine)

# 2. 检查队列积压
curl http://localhost:9090/metrics | grep queue_depth

# 3. 检查GC暂停 (jemalloc统计)
MALLOC_CONF=stats_print:true matching-engine

# 4. 查看系统日志
journalctl -u matching-engine -n 100 --no-pager

# 5. 性能分析
perf record -F 99 -p $(pgrep matching-engine) -g -- sleep 30
perf report
```

**解决方案**:
- 增加partition_count匹配CPU核心数
- 启用CPU亲和性
- 检查网络带宽和延迟
- 优化batch_size参数

#### 2. 内存不足

**症状**: OOM Killer或分配失败

```bash
# 检查内存使用
pmap -x $(pgrep matching-engine)

# 检查jemalloc统计
echo "stats_print" > /tmp/je_stats
MALLOC_CONF=stats_print:true matching-engine
```

**解决方案**:
- 减少queue_capacity
- 调整jemalloc参数: `dirty_decay_ms:5000`
- 增加物理内存

#### 3. 持久化失败

**症状**: WAL写入错误

```bash
# 检查磁盘空间
df -h /var/lib/matching-engine

# 检查文件句柄
lsof -p $(pgrep matching-engine) | wc -l

# 检查磁盘I/O
iostat -x 1
```

**解决方案**:
- 清理旧快照: `rm /var/lib/matching-engine/snapshots/*.old`
- 增加文件句柄限制: `ulimit -n 65536`
- 使用SSD或NVMe存储

### 日志分析

**关键日志模式**:

```bash
# 错误日志
grep "ERROR" /var/log/matching-engine/engine.log

# 慢查询 (>1ms)
grep "slow_order" /var/log/matching-engine/engine.log

# 连接错误
grep "connection.*failed" /var/log/matching-engine/engine.log
```

---

## ⚡ 性能优化

### 系统调优

**内核参数** (/etc/sysctl.conf):

```bash
# 网络优化
net.core.rmem_max=134217728
net.core.wmem_max=134217728
net.ipv4.tcp_rmem=4096 87380 67108864
net.ipv4.tcp_wmem=4096 65536 67108864

# 减少TCP延迟
net.ipv4.tcp_low_latency=1
net.ipv4.tcp_timestamps=0

# 增加文件句柄
fs.file-max=2097152

# NUMA优化
vm.zone_reclaim_mode=0

# 应用配置
sysctl -p
```

**CPU调度**:

```bash
# 设置CPU governor为performance
echo performance | sudo tee /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor

# 禁用CPU空闲状态 (减少唤醒延迟)
for i in /sys/devices/system/cpu/cpu*/cpuidle/state*/disable; do
  echo 1 > $i
done

# 禁用超线程 (可选，用于确定性延迟)
echo off > /sys/devices/system/cpu/smt/control
```

### 应用级优化

**环境变量**:

```bash
# jemalloc优化
export MALLOC_CONF="background_thread:true,narenas:16,dirty_decay_ms:5000"

# 线程池大小
export TOKIO_WORKER_THREADS=16

# 日志缓冲
export RUST_LOG_BUF_SIZE=8192
```

**配置参数**:

```toml
[partitioning]
partition_count = 16      # = CPU核心数
queue_capacity = 10000    # 根据内存调整
batch_size = 100          # 平衡延迟和吞吐

[network]
buffer_size = 8192        # 减少系统调用
queue_depth = 1024        # io_uring队列深度
```

---

## 🔒 安全加固

### 1. 网络安全

```bash
# 防火墙规则
sudo ufw allow 8080/tcp  # 服务端口
sudo ufw allow 9090/tcp  # 指标端口
sudo ufw enable

# TLS配置 (使用nginx反向代理)
server {
    listen 443 ssl;
    ssl_certificate /etc/ssl/certs/engine.crt;
    ssl_certificate_key /etc/ssl/private/engine.key;

    location / {
        proxy_pass http://localhost:8080;
    }
}
```

### 2. 访问控制

```bash
# 限制metrics端点 (仅内网访问)
iptables -A INPUT -p tcp --dport 9090 -s 10.0.0.0/8 -j ACCEPT
iptables -A INPUT -p tcp --dport 9090 -j DROP
```

### 3. 资源隔离

```bash
# cgroup限制
sudo cgcreate -g cpu,memory:/matching-engine
sudo cgset -r cpu.shares=8192 matching-engine
sudo cgset -r memory.limit_in_bytes=64G matching-engine

# 运行服务
sudo cgexec -g cpu,memory:matching-engine matching-engine
```

---

## 📦 备份恢复

### 数据备份

```bash
#!/bin/bash
# backup.sh

BACKUP_DIR="/backup/matching-engine/$(date +%Y%m%d)"
mkdir -p $BACKUP_DIR

# 停止服务
systemctl stop matching-engine

# 备份数据
tar -czf $BACKUP_DIR/wal.tar.gz /var/lib/matching-engine/wal
tar -czf $BACKUP_DIR/snapshots.tar.gz /var/lib/matching-engine/snapshots

# 启动服务
systemctl start matching-engine

# 上传到远程
rsync -avz $BACKUP_DIR user@backup-server:/backups/
```

### 灾难恢复

```bash
# 1. 恢复数据
tar -xzf wal.tar.gz -C /var/lib/matching-engine/
tar -xzf snapshots.tar.gz -C /var/lib/matching-engine/

# 2. 验证数据完整性
matching-engine --verify-wal /var/lib/matching-engine/wal

# 3. 从最新快照恢复
matching-engine --recover-from-snapshot \
  /var/lib/matching-engine/snapshots/latest.snapshot

# 4. 启动服务
systemctl start matching-engine
```

---

## 📞 支持联系

- **文档**: [README.md](README.md)
- **架构**: [ARCHITECTURE.md](ARCHITECTURE.md)
- **性能**: [PERFORMANCE_REPORT.md](PERFORMANCE_REPORT.md)
- **问题追踪**: GitHub Issues

---

**最后更新**: 2025-11-18
**版本**: v1.0
