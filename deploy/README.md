# Deployment templates

These are **templates**, not a complete packaging story.

- `trading.service` runs the implemented single-process runtime:
  `/opt/trading/bin/trading-engine run`.
- `trading-middleware` and `trading-core` are implemented split-process
  binaries. They share command/public/private ring files created by middleware:
  core opens the command consumer and event producers, middleware opens the
  event consumers.
- `trading-gateway` is a TCP relay: it accepts member frames and forwards them
  to the middleware gateway listener. Authentication and rate limiting still
  belong in a future gateway layer.
- `deploy/Dockerfile` builds a single-process runtime image; the compose example
  exposes 8000/tcp (gateway) and 9100/tcp (metrics).

## Install

```text
install -d /opt/trading/bin /etc/trading /run/trading /var/lib/trading
install -m 0644 deploy/trading.env.example /etc/trading/trading.env
install -m 0644 deploy/trading-*.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable trading-middleware trading-core trading-gateway
```

Start order:

```text
trading-middleware → trading-core → trading-gateway
```

## Notes

- `trading-core` should run on a dedicated CPU set in production.
- Shared-memory files live under `/run/trading` and must be recreated on boot.
- `ProtectSystem=strict` plus `ReadWritePaths` limits filesystem writes.
- Set `LimitMEMLOCK=infinity` if the deployment uses huge pages or mlocked rings.
- Prometheus should scrape the metrics listener on `127.0.0.1:9100/metrics`.
- No automatic core failover: use `Restart=on-failure` and operator replay.
