[← Back to README](../README.md)

# 🔧 Troubleshooting

### 🌐 App Pilot does not connect to Magician

```bash
# Verify Magician and the paired-device route are healthy.
make status-magician
make logs-magdroid
```

Check App Pilot's Connection accordion for the host and pairing credentials,
then confirm Accessibility and battery access. The phone dials out; WiFi LAN
reachability, a device IP, `adb forward`, and an inbound port are not involved.

---

### ♿ AccessibilityService not running

```bash
# Check current status
adb shell settings get secure enabled_accessibility_services

# Re-enable
adb shell settings put secure enabled_accessibility_services \
  ai.magicbeans.magican/ai.magicbeans.magdroid.service.MagdroidAccessibilityService
adb shell settings put secure accessibility_enabled 1
```

On Android 15+, you may also need to enable **"Allow restricted settings"** for Magican in Settings > Apps.

---

### 📸 Screenshots return fallback (ADB screencap)

MediaProjection requires a one-time user consent. Open App Pilot and grant Screen
capture, then tap "Start now" on the system dialog. On Android 14+, consent
resets when the app process dies.

---

### 🐌 High latency on first screenshot

The first screenshot after MediaProjection setup takes 150-300ms (warm-up). Subsequent screenshots run at ~60ms. This is a one-time cost per session.

---

### 💥 Companion app crashes or stops responding

```bash
# Check crash logs
adb logcat -s MagdroidBridge:V Magdroid:V

# Force restart
adb shell am force-stop ai.magicbeans.magican
adb shell am start -n ai.magicbeans.magican/ai.magicbeans.magdroid.MainActivity

# Re-enable AccessibilityService after restart
adb shell settings put secure enabled_accessibility_services \
  ai.magicbeans.magican/ai.magicbeans.magdroid.service.MagdroidAccessibilityService
```
