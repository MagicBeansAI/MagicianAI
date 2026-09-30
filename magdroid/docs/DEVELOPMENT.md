[← Back to README](../README.md)

# 🔨 Development Guide

### 🏗️ Building from source

```bash
make build-magdroid
make test-magdroid
make install-magdroid         # requires an attached device
```

---

### 📦 Project Structure

```
magdroid/
├── android/                       # Android app (runs on device)
│   ├── app/src/main/             # Compose product shell
│   ├── bridge/src/main/
│   │   ├── kotlin/.../magdroid/
│   │   │   ├── bridge/           # authenticated outbound WebSocket
│   │   │   ├── mcp/              # bounded MCP server, registry, handler
│   │   │   ├── service/          # AccessibilityService + command handler
│   │   │   ├── gesture/          # Gesture engine
│   │   │   ├── input/            # Text input, clipboard, key events
│   │   │   ├── screenshot/       # MediaProjection + libjpeg-turbo (JNI)
│   │   │   ├── uitree/           # UI tree walker + semantic extraction
│   │   │   └── notification/     # NotificationListenerService
│   │   ├── cpp/                   # JNI (libjpeg-turbo JPEG encoding)
│   │   └── res/                   # Resources
│   └── build.gradle.kts
├── docs/                          # Documentation
└── LICENSE                        # Apache 2.0
```

---

### 📋 Viewing companion app logs

```bash
make logs-magdroid
```
