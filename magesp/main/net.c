#include "net.h"
#include "audio.h"
#include "realtime.h"
#include "connection.h"
#include "cJSON.h"

#include <string.h>
#include <stdio.h>

#include "esp_event.h"
#include "esp_crt_bundle.h"
#include "esp_http_client.h"
#include "esp_http_server.h"
#include "esp_log.h"
#include "esp_mac.h"
#include "esp_netif.h"
#include "esp_system.h"
#include "esp_wifi.h"
#include "freertos/FreeRTOS.h"
#include "freertos/event_groups.h"
#include "freertos/task.h"
#include "freertos/semphr.h"
#include "nvs.h"
#include "nvs_flash.h"
#include "lwip/sockets.h"
#include "lwip/inet.h"

static const char *TAG = "net";

#define NVS_NS        "magesp"
#define K_SSID        "ssid"
#define K_PASS        "pass"
#define K_URL         "url"
#define K_THREAD      "thread"
#define K_CFID        "cfid"
#define K_CFSEC       "cfsec"
#define K_DEVTOK      "devtok"
#define K_CONNECTION  "connection_v1"
#define K_THEME       "theme"
#define K_MODE        "mode"
#define JOIN_ATTEMPTS 6
#define PROBE_PERIOD_MS 15000

static net_state_t s_state = NET_BOOTING;
static char        s_detail[96] = "";
static char        s_ssid[33]   = "";
static char        s_pass[65]   = "";
static char        s_ap[24]     = "";
static char        s_appw[16]   = "";
static char        s_ip[16]     = "";
/* Cloudflare Access service token. A tunnel fronted by Access answers a
   browser login redirect to anything that cannot sign in, which is every
   headless device; a service token is the mechanism built for this case. */
static char        s_devid[24]  = "";
static connection_profile_t s_connection = {.version = 1, .thread = "general"};
#define s_url    (s_connection.url)
#define s_thread (s_connection.thread)
#define s_cfid   (s_connection.cfid)
#define s_cfsec  (s_connection.cfsec)
#define s_devtok (s_connection.token)
static SemaphoreHandle_t s_config_lock;
static volatile bool s_restart_pending;
static bool        s_dark       = false;
static voice_mode_t s_mode      = MODE_DICTATE;

/* Scan results captured once, before a client attaches. */
#define MAX_APS 14
static char s_aps[MAX_APS][33];
static int  s_ap_count;
static int         s_attempts   = 0;
static bool        s_provisioning = false;
/* Two TLS sessions do not fit in this heap. The probe yields to a live turn
   rather than competing with it for the memory a handshake needs. */
static volatile bool s_busy = false;
static EventGroupHandle_t s_ev;
#define BIT_GOT_IP  BIT0
#define BIT_FAILED  BIT1

net_state_t net_state(void)  { return s_state; }
const char *net_detail(void) { return s_detail; }
const char *net_ip(void)        { return s_ip; }
const char *net_base_url(void)  { return s_url; }
const char *net_thread(void)    { return s_thread; }
bool        net_configured(void){ return s_url[0] != 0; }
void        net_set_busy(bool b) { s_busy = b; }
const char *net_cf_id(void)     { return s_cfid; }
const char *net_cf_secret(void) { return s_cfsec; }
bool        net_has_cf_token(void) { return s_cfid[0] && s_cfsec[0]; }
const char *net_device_id(void)     { return s_devid; }
const char *net_device_token(void)  { return s_devtok; }
bool        net_has_device_token(void) { return s_devtok[0] != 0; }
bool        net_dark_theme(void)       { return s_dark; }
voice_mode_t net_voice_mode(void)      { return s_mode; }

const char *net_voice_mode_name(voice_mode_t m)
{
    switch (m) {
    case MODE_HANDSFREE: return "hands free";
    case MODE_REALTIME:  return "realtime";
    default:             return "dictate";
    }
}

/* All three modes are selectable. This is UI availability, not a check that
   the selected backend can provide a mode; realtime remains experimental. */
bool net_voice_mode_available(voice_mode_t m) { (void)m; return true; }

void net_set_voice_mode(voice_mode_t m)
{
    if (m == s_mode) return;
    s_mode = m;
    nvs_handle_t h;
    if (nvs_open(NVS_NS, NVS_READWRITE, &h) == ESP_OK) {
        nvs_set_u8(h, K_MODE, (uint8_t)m);
        nvs_commit(h);
        nvs_close(h);
    }
    ESP_LOGI(TAG, "voice mode -> %s%s", net_voice_mode_name(m),
             net_voice_mode_available(m) ? "" : " (not built yet)");
}

static void set_state(net_state_t st, const char *fmt, ...)
{
    s_state = st;
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(s_detail, sizeof s_detail, fmt, ap);
    va_end(ap);
    ESP_LOGI(TAG, "state=%d  %s", (int)st, s_detail);
}

/* ------------------------------------------------------------------ nvs */

static esp_err_t connection_store(nvs_handle_t h, const connection_profile_t *profile)
{
    if (!connection_valid(profile)) return ESP_ERR_INVALID_ARG;
    esp_err_t error = nvs_set_blob(h, K_CONNECTION, profile, sizeof *profile);
    if (error == ESP_OK) error = nvs_commit(h);
    return error;
}

static bool creds_load(void)
{
    nvs_handle_t h;
    if (nvs_open(NVS_NS, NVS_READONLY, &h) != ESP_OK) return false;
    size_t n = sizeof s_ssid;
    bool ok = (nvs_get_str(h, K_SSID, s_ssid, &n) == ESP_OK) && s_ssid[0];
    n = sizeof s_pass; if (nvs_get_str(h, K_PASS, s_pass, &n) != ESP_OK) s_pass[0] = 0;
    n = sizeof s_connection;
    esp_err_t profile_error = nvs_get_blob(h, K_CONNECTION, &s_connection, &n);
    if (profile_error == ESP_ERR_NVS_NOT_FOUND) {
        /* Read old firmware's fields once. Future writes use one blob so a
           reset cannot join a new URL to the old URL's bearer or Access pair. */
        n = sizeof s_url; if (nvs_get_str(h, K_URL, s_url, &n) != ESP_OK) s_url[0] = 0;
        n = sizeof s_thread; nvs_get_str(h, K_THREAD, s_thread, &n);
        n = sizeof s_cfid; if (nvs_get_str(h, K_CFID, s_cfid, &n) != ESP_OK) s_cfid[0] = 0;
        n = sizeof s_cfsec; if (nvs_get_str(h, K_CFSEC, s_cfsec, &n) != ESP_OK) s_cfsec[0] = 0;
        n = sizeof s_devtok; if (nvs_get_str(h, K_DEVTOK, s_devtok, &n) != ESP_OK) s_devtok[0] = 0;
        char normalized[sizeof s_url];
        if (s_url[0] && connection_origin(s_url, normalized, sizeof normalized))
            strcpy(s_url, normalized);
    } else if (profile_error != ESP_OK || n != sizeof s_connection) {
        memset(&s_connection, 0, sizeof s_connection);
    }
    if (!connection_valid(&s_connection)) {
        ESP_LOGE(TAG, "stored connection is invalid; configure the backend again");
        s_connection = (connection_profile_t){.version = 1, .thread = "general"};
    }
    char theme[8] = "";
    n = sizeof theme;
    if (nvs_get_str(h, K_THEME, theme, &n) == ESP_OK) s_dark = (theme[0] == 'd');
    nvs_close(h);
    if (profile_error == ESP_ERR_NVS_NOT_FOUND && s_url[0]) {
        if (nvs_open(NVS_NS, NVS_READWRITE, &h) == ESP_OK) {
            if (connection_store(h, &s_connection) != ESP_OK)
                ESP_LOGW(TAG, "could not migrate the stored connection");
            nvs_close(h);
        }
    }
    return ok;
}

/* Theme is independent of pairing or wifi, so it is read on its own. */
static void theme_load(void)
{
    nvs_handle_t h;
    if (nvs_open(NVS_NS, NVS_READONLY, &h) != ESP_OK) return;
    char theme[8] = "";
    size_t n = sizeof theme;
    if (nvs_get_str(h, K_THEME, theme, &n) == ESP_OK) s_dark = (theme[0] == 'd');
    uint8_t mode = 0;
    if (nvs_get_u8(h, K_MODE, &mode) == ESP_OK && mode <= MODE_REALTIME)
        s_mode = (voice_mode_t)mode;
    nvs_close(h);
}

static esp_err_t creds_save(const char *ssid, const char *pass,
                            const connection_profile_t *profile)
{
    nvs_handle_t h;
    esp_err_t error = nvs_open(NVS_NS, NVS_READWRITE, &h);
    if (error != ESP_OK) return error;
    error = nvs_set_str(h, K_SSID, ssid);
    if (error == ESP_OK) error = nvs_set_str(h, K_PASS, pass);
    if (error == ESP_OK) error = connection_store(h, profile);
    nvs_close(h);
    return error;
}

/* ------------------------------------------------------------- form http */

static bool form_field(const char *body, const char *key, char *out, size_t cap)
{
    return connection_form_get(body, key, out, cap) == 1;
}

static bool read_form(httpd_req_t *req, char *body, size_t capacity)
{
    if (req->content_len >= capacity) {
        httpd_resp_set_status(req, "413 Payload Too Large");
        httpd_resp_sendstr(req, "configuration is too long");
        return false;
    }
    size_t offset = 0;
    while (offset < req->content_len) {
        int n = httpd_req_recv(req, body + offset, req->content_len - offset);
        if (n <= 0) {
            httpd_resp_set_status(req, "400 Bad Request");
            httpd_resp_sendstr(req, "incomplete configuration");
            return false;
        }
        offset += (size_t)n;
    }
    body[offset] = 0;
    return true;
}

static bool config_authorized(httpd_req_t *req, const char *body)
{
    char auth[32];
    if (!form_field(body, "auth", auth, sizeof auth) || strcmp(auth, s_appw)) {
        httpd_resp_set_status(req, "403 Forbidden");
        httpd_resp_sendstr(req, "enter the device passphrase shown on its screen");
        return false;
    }
    return true;
}

/* Caller holds s_config_lock. The running connection stays immutable until
   restart; an old in-flight request can never pick up the new destination. */
static bool prepare_form_profile(httpd_req_t *req, const char *body, connection_profile_t *next)
{
    char url[128], thread[48], cfid[80], cfsec[96], repair[8];
    int u = connection_form_get(body, "url", url, sizeof url);
    int t = connection_form_get(body, "thread", thread, sizeof thread);
    int i = connection_form_get(body, "cfid", cfid, sizeof cfid);
    int s = connection_form_get(body, "cfsec", cfsec, sizeof cfsec);
    int r = connection_form_get(body, "repair", repair, sizeof repair);
    if (u < 0 || t < 0 || i < 0 || s < 0 || r < 0
        || (r && strcmp(repair, "0") && strcmp(repair, "1"))
        || !connection_prepare(&s_connection, u ? url : NULL, t ? thread : NULL,
                               i ? cfid : NULL, s ? cfsec : NULL,
                               r && !strcmp(repair, "1"), next)) {
        httpd_resp_set_status(req, "400 Bad Request");
        httpd_resp_sendstr(req, "use a base http(s) URL and a complete Access credential pair");
        return false;
    }
    return true;
}

static bool config_idle(httpd_req_t *req)
{
    if (s_restart_pending || s_busy || audio_capturing() || audio_streaming() || audio_speaker_busy()) {
        httpd_resp_set_status(req, "409 Conflict");
        httpd_resp_sendstr(req, "finish the active voice turn or restart before changing the connection");
        return false;
    }
    return true;
}

/* Scanning moves the radio off the AP's channel for seconds at a time, which
   drops every attached client. Doing it inside the page handler meant the
   connection died exactly when someone tried to load the form. Scan once,
   before anyone is attached, and serve the cached list. */
static void scan_once(void)
{
    wifi_scan_config_t sc = {0};
    if (esp_wifi_scan_start(&sc, true) != ESP_OK) return;
    uint16_t n = 0;
    esp_wifi_scan_get_ap_num(&n);
    if (n > MAX_APS) n = MAX_APS;
    wifi_ap_record_t recs[MAX_APS];
    if (n && esp_wifi_scan_get_ap_records(&n, recs) == ESP_OK) {
        s_ap_count = 0;
        for (uint16_t i = 0; i < n; i++) {
            if (recs[i].ssid[0] == 0) continue;
            strncpy(s_aps[s_ap_count], (char *)recs[i].ssid, sizeof s_aps[0] - 1);
            s_aps[s_ap_count][sizeof s_aps[0] - 1] = 0;
            s_ap_count++;
        }
    }
    ESP_LOGI(TAG, "scanned %d networks", s_ap_count);
}

static esp_err_t get_root(httpd_req_t *req)
{
    ESP_LOGI(TAG, "HTTP GET / -- request reached the device");

    httpd_resp_set_type(req, "text/html");
    httpd_resp_sendstr_chunk(req,
        "<!doctype html><meta name=viewport content='width=device-width,initial-scale=1'>"
        "<title>magesp setup</title><style>"
        "body{font:16px system-ui;margin:0;padding:24px;background:#f3f5fa;color:#39456f}"
        "h1{font-size:20px}label{display:block;margin:14px 0 4px;font-weight:600}"
        "input,select{width:100%;box-sizing:border-box;padding:12px;font-size:16px;"
        "border:1px solid #cfd022;border-color:#cdd4e6;border-radius:8px;background:#fff}"
        "button{margin-top:20px;width:100%;padding:14px;font-size:16px;border:0;"
        "border-radius:8px;background:#39456f;color:#fff}"
        "p{color:#5a6488;font-size:14px}</style>"
        "<h1>magesp setup</h1><form method=POST action=/save>"
        "<label>Device passphrase (shown on its screen)</label>"
        "<input name=auth type=password required autocomplete=off>"
        "<label>Network</label><select name=ssid>");
    for (int i = 0; i < s_ap_count; i++) {
        char row[128];
        snprintf(row, sizeof row, "<option value=\"%s\">%s</option>",
                 s_aps[i], s_aps[i]);
        httpd_resp_sendstr_chunk(req, row);
    }
    if (s_ap_count == 0)
        httpd_resp_sendstr_chunk(req,
            "<option value=''>(no scan -- type the name below)</option>");
    httpd_resp_sendstr_chunk(req,
        "</select>"
        "<label>or type the network name</label>"
        "<input name=ssid2 placeholder='leave blank to use the list above'>"
        "<label>Password</label><input name=pass type=password autocomplete=off>"
        "<label>Magician URL</label>"
        "<input name=url placeholder='http://192.168.1.20:3002'>"
        "<label>Thread</label><input name=thread value='general'>"
        "<label>Theme</label><select name=theme>"
        "<option value=light>light</option>"
        "<option value=dark>dark (uses less power)</option></select>"
        "<p>Only if the URL is behind Cloudflare Access:</p>"
        "<label>Access client id</label><input name=cfid placeholder='optional'>"
        "<label>Access client secret</label><input name=cfsec type=password placeholder='optional'>"
        "<button>Save and restart</button></form>"
        "<p>Settings are stored on the device. It restarts and joins your network.</p>");
    httpd_resp_sendstr_chunk(req, NULL);
    return ESP_OK;
}

static esp_err_t get_help(httpd_req_t *req)
{
    httpd_resp_set_type(req, "text/html");
    httpd_resp_sendstr_chunk(req,
      "<!doctype html><meta name=viewport content='width=device-width,initial-scale=1'>"
      "<title>magesp help</title><style>"
      "body{font:16px/1.55 system-ui;margin:0;padding:20px;background:#f3f5fa;color:#39456f}"
      "h1{font-size:22px;margin:0 0 4px}h2{font-size:17px;margin:26px 0 8px}"
      "code{background:#e6eaf6;padding:2px 6px;border-radius:5px;font-size:14px;"
      "word-break:break-all}"
      "table{width:100%;border-collapse:collapse;margin:8px 0;font-size:14px}"
      "td,th{border-bottom:1px solid #dbe1f0;padding:7px 4px;text-align:left;"
      "vertical-align:top}th{color:#5a6488;font-weight:600}"
      "ol{padding-left:20px}li{margin:6px 0}"
      ".s{background:#fff;border:1px solid #dbe1f0;border-radius:10px;padding:12px;"
      "margin:12px 0}.k{color:#5a6488;font-size:13px}"
      "</style><h1>magesp</h1>"
      "<div class=k>Magician voice terminal &mdash; setup and reference</div>");

    char box[512];
    snprintf(box, sizeof box,
      "<div class=s><b>This device right now</b><br>"
      "<span class=k>state</span> %s<br>"
      "<span class=k>address</span> %s<br>"
      "<span class=k>magician</span> %s</div>",
      s_detail[0] ? s_detail : "-",
      s_ip[0] ? s_ip : "192.168.4.1 (setup mode)",
      s_url[0] ? s_url : "not configured");
    httpd_resp_sendstr_chunk(req, box);

    httpd_resp_sendstr_chunk(req,
      "<h2>Connect it to Magician</h2><ol>"
      "<li>Join the device's wifi <code>magesp-XXXX</code>. The passphrase is on"
      " the device screen and stays the same across reboots.</li>"
      "<li>Open <code>http://192.168.4.1</code> and pick your network.</li>"
      "<li>Enter the <b>Magician base URL</b>, the device passphrase, and any"
      " required Access credentials, then save. It restarts and joins.</li>"
      "</ol>"
      "<h2>Which base URL</h2><table>"
      "<tr><th>Where</th><th>URL</th></tr>"
      "<tr><td>Same network as the server</td><td><code>http://SERVER-IP:3002</code></td></tr>"
      "<tr><td>Phone hotspot, or anywhere else</td>"
      "<td><code>https://your-tunnel-host</code></td></tr></table>"
      "<p class=k>A LAN address only resolves while device and server share a"
      " network. On a hotspot they do not, so a tunnel URL is required rather"
      " than merely tidier. https is supported: the public-CA bundle is built in.</p>"

      "<h2>What the device calls</h2><table>"
      "<tr><th>Endpoint</th><th>Why</th></tr>"
      "<tr><td><code>POST /api/magician/v2/devices/pair</code></td>"
      "<td>First connection; requires verified Access or server-local bootstrap</td></tr>"
      "<tr><td><code>GET /api/magician/v2/devices/me</code></td>"
      "<td>Verify this paired device and its scope, every 15s</td></tr>"
      "<tr><td><code>POST /api/magician/v2/media/voice-notes</code></td>"
      "<td>The whole turn: audio &rarr; transcript &rarr; reply</td></tr>"
      "<tr><td><code>POST /api/magician/v2/media/tts/synthesize</code></td>"
      "<td>Reply audio. Asks for <code>format: wav</code>; the streaming decoder"
      " reads the rate and converts PCM16 or float32 for the speaker.</td></tr>"
      "<tr><td><code>POST /api/magician/v2/media/voice-notes/events</code></td>"
      "<td>Lifecycle events</td></tr>"
      "<tr><td><code>GET /api/magician/v2/media/voice/{id}/control</code></td>"
      "<td>Realtime voice control and connection diagnostic</td></tr></table>"
      "<p class=k>Paired requests carry one opaque bearer whose server-side"
      " record fixes the principal and workspace. Put access control in front"
      " of any public tunnel.</p>"

      "<h2>Using it</h2><table>"
      "<tr><td><b>Short press BOOT</b></td><td>Cycle game &rarr; face &rarr;"
      " For you &rarr; voice mode. A press on a dark screen only wakes it.</td></tr>"
      "<tr><td><b>Hold BOOT on the face</b></td><td>Push to talk; release to send.</td></tr>"
      "<tr><td><b>Tap the face</b></td><td>Start or stop listening in hands-free"
      " mode. In push-to-talk mode, read the last answer.</td></tr>"
      "<tr><td><b>Game</b></td><td>Tilt to move; tapping does not move the ball.</td></tr>"
      "<tr><td><b>Enter the game</b></td><td>Re-levels tilt to how you are"
      " holding it</td></tr></table>"

      "<h2>When it will not connect</h2><table>"
      "<tr><th>Screen</th><th>Do this</th></tr>"
      "<tr><td>set up wifi</td><td>No credentials stored. Join"
      " <code>magesp-XXXX</code>.</td></tr>"
      "<tr><td>wifi failed</td><td>Wrong password, or the network is 5GHz."
      " This radio is <b>2.4GHz only</b> &mdash; on iPhone hotspots turn on"
      " <b>Maximize Compatibility</b>.</td></tr>"
      "<tr><td>magician unreachable</td><td>The URL is shown. Check the server"
      " is running, and that it binds <code>0.0.0.0</code> rather than"
      " <code>127.0.0.1</code> &mdash; a localhost bind is invisible to every"
      " other device and looks identical to being down.</td></tr>"
      "<tr><td>device access refused</td><td>Ask the backend owner to restore"
      " access, then use <code>/config</code> with the device passphrase and"
      " <code>repair=1</code> to pair again.</td></tr></table>"
      "<p class=k>Re-run setup any time at <code>/</code> on this address.</p>");
    httpd_resp_sendstr_chunk(req, NULL);
    return ESP_OK;
}

/* Hand the last recording back over HTTP.
 *
 * A capture path that feeds no audio looks identical to a working one from
 * every layer above it, so the only honest check is to take the bytes off the
 * device and listen. This is that door. */
static esp_err_t get_rec(httpd_req_t *req)
{
    FILE *f = fopen(audio_path(), "rb");
    if (f == NULL) {
        httpd_resp_set_status(req, "404 Not Found");
        httpd_resp_sendstr(req, "nothing recorded yet");
        return ESP_OK;
    }
    httpd_resp_set_type(req, "audio/wav");
    httpd_resp_set_hdr(req, "Content-Disposition", "attachment; filename=utterance.wav");

    char buf[1024];
    size_t n;
    while ((n = fread(buf, 1, sizeof buf, f)) > 0) {
        if (httpd_resp_send_chunk(req, buf, n) != ESP_OK) {
            fclose(f);
            return ESP_FAIL;
        }
    }
    fclose(f);
    httpd_resp_send_chunk(req, NULL, 0);
    return ESP_OK;
}

static esp_err_t post_save(httpd_req_t *req)
{
    char body[1024];
    if (!read_form(req, body, sizeof body) || !config_authorized(req, body)) return ESP_OK;
    char ssid[33], typed[33], pass[65];
    int a = connection_form_get(body, "ssid", ssid, sizeof ssid);
    int b = connection_form_get(body, "ssid2", typed, sizeof typed);
    int c = connection_form_get(body, "pass", pass, sizeof pass);
    if (a < 0 || b < 0 || c < 0) {
        httpd_resp_set_status(req, "400 Bad Request");
        httpd_resp_sendstr(req, "invalid network configuration");
        return ESP_OK;
    }
    if (b && typed[0]) strcpy(ssid, typed);
    if (!ssid[0]) {
        httpd_resp_set_status(req, "400 Bad Request");
        httpd_resp_sendstr(req, "network is required");
        return ESP_OK;
    }
    xSemaphoreTake(s_config_lock, portMAX_DELAY);
    connection_profile_t next;
    if (!config_idle(req) || !prepare_form_profile(req, body, &next)) {
        xSemaphoreGive(s_config_lock);
        return ESP_OK;
    }
    esp_err_t error = creds_save(ssid, pass, &next);
    if (error == ESP_OK) s_restart_pending = true;
    xSemaphoreGive(s_config_lock);
    if (error != ESP_OK) {
        httpd_resp_set_status(req, "500 Internal Server Error");
        httpd_resp_sendstr(req, "could not save configuration; connection was not activated");
        return ESP_OK;
    }
    char theme[8];
    if (form_field(body, "theme", theme, sizeof theme) && theme[0]) {
        nvs_handle_t h;
        if (nvs_open(NVS_NS, NVS_READWRITE, &h) == ESP_OK) {
            nvs_set_str(h, K_THEME, theme[0] == 'd' ? "dark" : "light");
            nvs_commit(h);
            nvs_close(h);
        }
    }
    httpd_resp_set_type(req, "text/html");
    httpd_resp_sendstr(req,
        "<meta name=viewport content='width=device-width,initial-scale=1'>"
        "<body style=\"font:16px system-ui;padding:24px;background:#f3f5fa;color:#39456f\">"
        "<h1>Saved</h1><p>Restarting and verifying the selected backend.</p>");
    vTaskDelay(pdMS_TO_TICKS(900));
    esp_restart();
    return ESP_OK;
}

/* Minimal DNS responder: every question is answered with our own address.
 *
 * This is what makes Android usable. Android probes a known URL to decide
 * whether a network has internet; when that probe fails outright it treats
 * the network as useless and keeps routing over mobile data, so the setup
 * page is unreachable no matter how correct the HTTP server is. Hijacking
 * DNS makes the probe land on us, our 302 tells Android "sign in required",
 * and Android then opens the page in a browser BOUND to this network. */
static void dns_task(void *arg)
{
    const int sock = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
    if (sock < 0) { ESP_LOGE(TAG, "dns socket failed"); vTaskDelete(NULL); return; }

    struct sockaddr_in me = {
        .sin_family = AF_INET, .sin_port = htons(53), .sin_addr.s_addr = htonl(INADDR_ANY),
    };
    if (bind(sock, (struct sockaddr *)&me, sizeof me) < 0) {
        ESP_LOGE(TAG, "dns bind failed");
        close(sock);
        vTaskDelete(NULL);
        return;
    }
    ESP_LOGI(TAG, "captive dns up on :53");

    uint8_t buf[512];
    while (s_provisioning) {
        struct sockaddr_in from;
        socklen_t flen = sizeof from;
        const int n = recvfrom(sock, buf, sizeof buf, 0, (struct sockaddr *)&from, &flen);
        if (n < 12) continue;

        /* Answer only single-question A-style lookups; ignore the rest. */
        if ((buf[2] & 0x80) != 0) continue;          /* already a response  */
        if (buf[4] != 0 || buf[5] != 1) continue;    /* QDCOUNT must be 1   */

        int q = 12;                                   /* walk the QNAME     */
        while (q < n && buf[q] != 0) q += buf[q] + 1;
        q += 5;                                       /* null + QTYPE+CLASS */
        if (q > n || q + 16 > (int)sizeof buf) continue;

        buf[2] = 0x85; buf[3] = 0x80;                 /* QR|AA|RD, RA       */
        buf[6] = 0; buf[7] = 1;                       /* ANCOUNT = 1        */
        buf[8] = 0; buf[9] = 0; buf[10] = 0; buf[11] = 0;

        uint8_t *a = buf + q;
        a[0] = 0xC0; a[1] = 0x0C;                     /* pointer to QNAME   */
        a[2] = 0; a[3] = 1;                           /* type A             */
        a[4] = 0; a[5] = 1;                           /* class IN           */
        a[6] = 0; a[7] = 0; a[8] = 0; a[9] = 60;      /* TTL 60s            */
        a[10] = 0; a[11] = 4;                         /* RDLENGTH           */
        a[12] = 192; a[13] = 168; a[14] = 4; a[15] = 1;

        sendto(sock, buf, q + 16, 0, (struct sockaddr *)&from, flen);
    }
    close(sock);
    vTaskDelete(NULL);
}

static esp_err_t redirect_to_form(httpd_req_t *req, httpd_err_code_t err)
{
    (void)err;
    ESP_LOGI(TAG, "HTTP %s -> 302 to the form (captive portal)", req->uri);
    httpd_resp_set_status(req, "302 Found");
    httpd_resp_set_hdr(req, "Location", "http://192.168.4.1/");
    httpd_resp_send(req, NULL, 0);
    return ESP_OK;
}

/* Set where Magician lives, without re-entering wifi.
 *
 * The hostname is never compiled in: the phones learn it from an enrollment
 * QR, and this device learns it here. Only the fields present in the request
 * are touched. A new origin discards the previous origin's credentials;
 * connection changes are saved together and activated by a restart.
 *
 * Guarded by the same passphrase shown on the device's screen, which keeps
 * reconfiguration to whoever can see the hardware -- the trust boundary the
 * setup AP already uses. */
static esp_err_t post_config(httpd_req_t *req)
{
    char body[1024];
    if (!read_form(req, body, sizeof body) || !config_authorized(req, body)) return ESP_OK;
    xSemaphoreTake(s_config_lock, portMAX_DELAY);
    connection_profile_t next;
    if (!prepare_form_profile(req, body, &next)) {
        xSemaphoreGive(s_config_lock);
        return ESP_OK;
    }
    bool restarting = memcmp(&next, &s_connection, sizeof next) != 0;
    if (restarting) {
        char ignored[16];
        if (!config_idle(req)) {
            xSemaphoreGive(s_config_lock);
            return ESP_OK;
        }
        if (connection_form_get(body, "rt_call", ignored, sizeof ignored) != 0
            || connection_form_get(body, "rt_spike", ignored, sizeof ignored) != 0) {
            xSemaphoreGive(s_config_lock);
            httpd_resp_set_status(req, "400 Bad Request");
            httpd_resp_sendstr(req, "change the connection before starting a call");
            return ESP_OK;
        }
        nvs_handle_t h;
        esp_err_t error = nvs_open(NVS_NS, NVS_READWRITE, &h);
        if (error == ESP_OK) {
            error = connection_store(h, &next);
            nvs_close(h);
        }
        if (error != ESP_OK) {
            xSemaphoreGive(s_config_lock);
            httpd_resp_set_status(req, "500 Internal Server Error");
            httpd_resp_sendstr(req, "could not save the connection");
            return ESP_OK;
        }
        s_restart_pending = true;
    }
    xSemaphoreGive(s_config_lock);

    /* Start or end a realtime call without a finger on the glass. The face tap
       is the real control; this exists so a call can be driven from a laptop
       while the heap and audio numbers are being read off the serial log. */
    char call[8] = "";
    if (form_field(body, "rt_call", call, sizeof call) && call[0]) {
        if (call[0] == '0') realtime_stop();
        else                realtime_start();
    }

    /* Realtime step 1: open a control WS, hold it, and report what it costs
       in heap. Deliberately a one-shot diagnostic rather than a mode -- it
       changes no stored setting and the turn path never calls it. */
    char spike[8] = "";
    if (form_field(body, "rt_spike", spike, sizeof spike) && spike[0]) {
        const int secs = atoi(spike);
        ESP_LOGI(TAG, "/config: realtime spike for %d s", secs);
        realtime_spike(secs);
    }

    /* Same rule as the screen: an unavailable mode is refused, not stored. */
    char mode[16] = "";
    if (form_field(body, "mode", mode, sizeof mode) && mode[0]) {
        for (int i = 0; i < 3; i++) {
            if (strcmp(mode, net_voice_mode_name((voice_mode_t)i)) != 0) continue;
            if (net_voice_mode_available((voice_mode_t)i)) net_set_voice_mode((voice_mode_t)i);
            else ESP_LOGW(TAG, "/config refused mode %s: not available here", mode);
            break;
        }
    }

    char theme[8] = "";
    if (form_field(body, "theme", theme, sizeof theme) && theme[0]) {
        s_dark = (theme[0] == 'd');
        nvs_handle_t th;
        if (nvs_open(NVS_NS, NVS_READWRITE, &th) == ESP_OK) {
            nvs_set_str(th, K_THEME, s_dark ? "dark" : "light");
            nvs_commit(th);
            nvs_close(th);
        }
        ESP_LOGI(TAG, "theme -> %s", s_dark ? "dark" : "light");
    }

    cJSON *response = cJSON_CreateObject();
    if (response) {
        cJSON_AddStringToObject(response, "url", next.url);
        cJSON_AddStringToObject(response, "thread", next.thread);
        cJSON_AddBoolToObject(response, "access_token", next.cfid[0] && next.cfsec[0]);
        cJSON_AddStringToObject(response, "mode", net_voice_mode_name(s_mode));
        cJSON_AddBoolToObject(response, "restarting", restarting);
        cJSON_AddStringToObject(response, "realtime_state", realtime_state_name());
        cJSON_AddStringToObject(response, "realtime_error", realtime_error());
        cJSON_AddNumberToObject(response, "realtime_seconds", realtime_seconds());
        cJSON_AddNumberToObject(response, "realtime_uplink_bytes", realtime_uplink_bytes());
        cJSON_AddNumberToObject(response, "realtime_uplink_drops", realtime_uplink_drops());
        cJSON_AddNumberToObject(response, "realtime_downlink_bytes", realtime_downlink_bytes());
        cJSON_AddNumberToObject(response, "realtime_responses", realtime_responses());
        char *encoded = cJSON_PrintUnformatted(response);
        httpd_resp_set_type(req, "application/json");
        if (encoded) httpd_resp_sendstr(req, encoded);
        else httpd_resp_sendstr(req, "{}");
        cJSON_free(encoded);
        cJSON_Delete(response);
    } else httpd_resp_sendstr(req, "{}");
    if (restarting) {
        vTaskDelay(pdMS_TO_TICKS(900));
        esp_restart();
    }

    return ESP_OK;
}

static void serve_http(void)
{
    httpd_config_t cfg = HTTPD_DEFAULT_CONFIG();
    cfg.lru_purge_enable = true;
    httpd_handle_t srv = NULL;
    if (httpd_start(&srv, &cfg) != ESP_OK) { ESP_LOGE(TAG, "httpd failed"); return; }
    httpd_uri_t root = {.uri = "/",     .method = HTTP_GET,  .handler = get_root};
    httpd_uri_t help = {.uri = "/help", .method = HTTP_GET,  .handler = get_help};
    httpd_uri_t rec  = {.uri = "/rec.wav", .method = HTTP_GET, .handler = get_rec};
    httpd_uri_t save = {.uri = "/save", .method = HTTP_POST, .handler = post_save};
    httpd_uri_t conf = {.uri = "/config", .method = HTTP_POST, .handler = post_config};
    httpd_register_uri_handler(srv, &root);
    httpd_register_uri_handler(srv, &help);
    httpd_register_uri_handler(srv, &rec);
    httpd_register_uri_handler(srv, &conf);
    httpd_register_uri_handler(srv, &save);
    if (s_provisioning)
        httpd_register_err_handler(srv, HTTPD_404_NOT_FOUND, redirect_to_form);
    ESP_LOGI(TAG, "http server up on port %d", cfg.server_port);

    /* Fetch our own page over the loopback. This splits "the server is not
       accepting" from "the client never sent", which no amount of staring at
       the client can do. */
    esp_http_client_config_t self = {
        .url = "http://127.0.0.1/", .method = HTTP_METHOD_GET, .timeout_ms = 3000,
    };
    esp_http_client_handle_t c = esp_http_client_init(&self);
    if (c != NULL) {
        const esp_err_t e = esp_http_client_perform(c);
        ESP_LOGI(TAG, "SELF-TEST http://127.0.0.1/ -> %s, status %d",
                 esp_err_to_name(e), esp_http_client_get_status_code(c));
        esp_http_client_cleanup(c);
    }
}

/* ------------------------------------------------------------------ wifi */

static void on_wifi(void *arg, esp_event_base_t base, int32_t id, void *data)
{
    /* While provisioning we are an AP with a STA interface kept only for
       scanning. Letting the STA half auto-connect makes it retry a network it
       has no config for, and the resulting churn stops the AP beaconing --
       which is exactly how the setup network becomes invisible. */
    if (s_provisioning && base == WIFI_EVENT &&
        (id == WIFI_EVENT_STA_START || id == WIFI_EVENT_STA_DISCONNECTED)) {
        return;
    }

    if (base == WIFI_EVENT && id == WIFI_EVENT_AP_STACONNECTED) {
        const wifi_event_ap_staconnected_t *e = (const wifi_event_ap_staconnected_t *)data;
        ESP_LOGI(TAG, "AP: client associated %02x:%02x:%02x:%02x:%02x:%02x",
                 e->mac[0], e->mac[1], e->mac[2], e->mac[3], e->mac[4], e->mac[5]);
        return;
    }
    if (base == WIFI_EVENT && id == WIFI_EVENT_AP_STADISCONNECTED) {
        ESP_LOGW(TAG, "AP: client left");
        return;
    }
    if (base == IP_EVENT && id == IP_EVENT_AP_STAIPASSIGNED) {
        const ip_event_ap_staipassigned_t *e = (const ip_event_ap_staipassigned_t *)data;
        ESP_LOGI(TAG, "AP: leased " IPSTR " -- browse http://192.168.4.1/",
                 IP2STR(&e->ip));
        return;
    }

    if (base == WIFI_EVENT && id == WIFI_EVENT_STA_START) {
        esp_wifi_connect();
    } else if (base == WIFI_EVENT && id == WIFI_EVENT_STA_DISCONNECTED) {
        if (++s_attempts <= JOIN_ATTEMPTS) {
            set_state(NET_CONNECTING, "joining %s (%d/%d)", s_ssid, s_attempts, JOIN_ATTEMPTS);
            esp_wifi_connect();
        } else {
            xEventGroupSetBits(s_ev, BIT_FAILED);
        }
    } else if (base == IP_EVENT && id == IP_EVENT_STA_GOT_IP) {
        const ip_event_got_ip_t *e = (const ip_event_got_ip_t *)data;
        snprintf(s_ip, sizeof s_ip, IPSTR, IP2STR(&e->ip_info.ip));
        s_attempts = 0;
        xEventGroupSetBits(s_ev, BIT_GOT_IP);
    }
}

static void start_ap(void)
{
    s_provisioning = true;
    /* name + passphrase are derived in net_start() */

    esp_netif_create_default_wifi_ap();
    esp_netif_create_default_wifi_sta();          /* APSTA so we can scan */
    wifi_config_t ap = {0};
    strncpy((char *)ap.ap.ssid, s_ap, sizeof ap.ap.ssid - 1);
    ap.ap.ssid_len       = strlen(s_ap);
    /* The setup form carries Wi-Fi credentials, so the AP uses WPA2.
     * Its existing MAC-derived passphrase stays stable across reboots, but
     * is not a random secret or proof of physical ownership. */
    /* passphrase already derived in net_start() */
    strncpy((char *)ap.ap.password, s_appw, sizeof ap.ap.password - 1);
    ap.ap.max_connection = 2;
    ap.ap.authmode       = WIFI_AUTH_WPA2_PSK;
    ap.ap.channel        = 1;
    ap.ap.ssid_hidden    = 0;
    ap.ap.beacon_interval = 100;

    ESP_ERROR_CHECK(esp_wifi_set_mode(WIFI_MODE_APSTA));
    ESP_ERROR_CHECK(esp_wifi_set_config(WIFI_IF_AP, &ap));
    ESP_ERROR_CHECK(esp_wifi_start());
    scan_once();      /* before any client attaches */
    serve_http();
    xTaskCreate(dns_task, "dns", 3072, NULL, 4, NULL);
    set_state(NET_PROVISIONING, "%s   pw %s", s_ap, s_appw);
}

/* A reachable /health is not authenticated readiness. Check the actual
   device identity and scope, without following redirects with credentials. */
static bool backend_reachable(int *status_out)
{
    if (!s_url[0] || !s_devtok[0]) return false;
    char probe[192];
    snprintf(probe, sizeof probe, "%s/api/magician/v2/devices/me", s_url);
    esp_http_client_config_t c = {
        .url = probe, .method = HTTP_METHOD_GET, .timeout_ms = 6000,
        .crt_bundle_attach = esp_crt_bundle_attach,
        .disable_auto_redirect = true,
    };
    esp_http_client_handle_t cl = esp_http_client_init(&c);
    if (!cl) return false;
    if (net_has_cf_token()) {
        esp_http_client_set_header(cl, "CF-Access-Client-Id", s_cfid);
        esp_http_client_set_header(cl, "CF-Access-Client-Secret", s_cfsec);
    }
    char bearer[112];
    snprintf(bearer, sizeof bearer, "Bearer %s", s_devtok);
    esp_http_client_set_header(cl, "Authorization", bearer);
    esp_http_client_set_header(cl, "X-Magician-Device-Id", s_devid);
    esp_http_client_set_header(cl, "Accept", "application/json");
    bool ready = false;
    if (esp_http_client_open(cl, 0) == ESP_OK) {
        esp_http_client_fetch_headers(cl);
        const int status = esp_http_client_get_status_code(cl);
        if (status_out) *status_out = status;
        char body[768];
        int n = esp_http_client_read_response(cl, body, sizeof body - 1);
        if (n > 0) {
            body[n] = 0;
            ready = connection_probe_valid(status, body, (size_t)n, s_devid);
        }
    }
    esp_http_client_cleanup(cl);
    return ready;
}

/* Register once with Magician and keep the token.
 *
 * A rejected bearer is retained until the owner explicitly requests repair;
 * revocation must not immediately bootstrap a replacement credential. */
void net_pair_if_needed(void)
{
    if (!net_configured() || s_devtok[0] || s_restart_pending) return;

    char url[192];
    snprintf(url, sizeof url, "%s/api/magician/v2/devices/pair", s_url);
    char body[192];
    const int len = snprintf(body, sizeof body,
        "{\"device_id\":\"%s\",\"label\":\"ESP32-C6 voice terminal\"}", s_devid);

    esp_http_client_config_t c = {
        .url = url, .method = HTTP_METHOD_POST, .timeout_ms = 10000,
        .crt_bundle_attach = esp_crt_bundle_attach,
        .disable_auto_redirect = true,
    };
    esp_http_client_handle_t cl = esp_http_client_init(&c);
    if (cl == NULL) return;
    esp_http_client_set_header(cl, "Content-Type", "application/json");
    if (net_has_cf_token()) {
        esp_http_client_set_header(cl, "CF-Access-Client-Id", s_cfid);
        esp_http_client_set_header(cl, "CF-Access-Client-Secret", s_cfsec);
    }
    esp_http_client_set_post_field(cl, body, len);

    char resp[384] = "";
    if (esp_http_client_open(cl, len) == ESP_OK &&
        esp_http_client_write(cl, body, len) == len) {
        esp_http_client_fetch_headers(cl);
        const int st = esp_http_client_get_status_code(cl);
        const int n = esp_http_client_read_response(cl, resp, sizeof resp - 1);
        if (n > 0) resp[n] = 0;
        if (st >= 200 && st < 300) {
            char token[sizeof s_devtok];
            if (n > 0 && connection_pair_token(resp, (size_t)n, token, sizeof token)) {
                xSemaphoreTake(s_config_lock, portMAX_DELAY);
                if (!s_restart_pending) {
                    connection_profile_t next = s_connection;
                    strcpy(next.token, token);
                    nvs_handle_t h;
                    esp_err_t saved = nvs_open(NVS_NS, NVS_READWRITE, &h);
                    if (saved == ESP_OK) {
                        saved = connection_store(h, &next);
                        nvs_close(h);
                    }
                    if (saved == ESP_OK) {
                        /* Single-core C6: publish the complete bearer before
                           any caller can observe its nonempty prefix. */
                        taskENTER_CRITICAL(NULL);
                        memcpy(s_devtok, next.token, sizeof s_devtok);
                        taskEXIT_CRITICAL(NULL);
                        ESP_LOGI(TAG, "paired as %s (token stored, never logged)", s_devid);
                    } else ESP_LOGW(TAG, "pairing token could not be saved");
                }
                xSemaphoreGive(s_config_lock);
            } else ESP_LOGW(TAG, "pair succeeded but returned no valid token");

        } else {
            ESP_LOGW(TAG, "pair refused: HTTP %d", st);
        }
    } else {
        ESP_LOGW(TAG, "pair request could not be sent");
    }
    esp_http_client_cleanup(cl);
}

static void net_task(void *arg)
{
    set_state(NET_CONNECTING, "joining %s", s_ssid);
    const EventBits_t bits = xEventGroupWaitBits(s_ev, BIT_GOT_IP | BIT_FAILED,
                                                 pdFALSE, pdFALSE, portMAX_DELAY);
    if (bits & BIT_FAILED) {
        /* Fall back to the setup AP rather than sitting dark: without this a
           moved router means a reflash. */
        set_state(NET_NO_WIFI, "cannot join %s", s_ssid);
        vTaskDelay(pdMS_TO_TICKS(2500));
        esp_wifi_stop();
        start_ap();
        vTaskDelete(NULL);
        return;
    }

    for (;;) {
        if (s_busy || s_restart_pending) {   /* a turn owns the radio + heap */
            vTaskDelay(pdMS_TO_TICKS(1000));
            continue;
        }
        int st = 0;
        net_pair_if_needed();
        if (backend_reachable(&st)) {
            set_state(NET_ONLINE, "%s", s_url);
        }
        else if (!s_devtok[0])      set_state(NET_NO_BACKEND, "pairing unavailable; check backend Access setup");
        else if (st == 401 || st == 403)
            set_state(NET_NO_BACKEND, "device access refused; owner must repair connection");
        else if (st)                set_state(NET_NO_BACKEND, "%s -> HTTP %d", s_url, st);
        else                        set_state(NET_NO_BACKEND, "no answer: %s", s_url);
        vTaskDelay(pdMS_TO_TICKS(PROBE_PERIOD_MS));
    }
}

/* Device identity: AP name and the passphrase that also guards /config.
 * Derived from the MAC so both survive reboots and reflashes, and so the
 * passphrase on the screen is always the one that works. */
static void derive_identity(void)
{
    uint8_t mac[6] = {0};
    esp_read_mac(mac, ESP_MAC_WIFI_SOFTAP);
    snprintf(s_ap, sizeof s_ap, "magesp-%02X%02X", mac[4], mac[5]);
    snprintf(s_appw, sizeof s_appw, "%02u%02u%02u%02u",
             mac[2] % 100u, mac[3] % 100u, mac[4] % 100u, mac[5] % 100u);
    /* Stable across reboots and reflashes, so re-pairing does not litter the
       roster with a new entry every time the firmware changes. */
    snprintf(s_devid, sizeof s_devid, "magesp-%02X%02X%02X",
             mac[3], mac[4], mac[5]);
}

void net_start(void)
{
    esp_err_t e = nvs_flash_init();
    if (e == ESP_ERR_NVS_NO_FREE_PAGES || e == ESP_ERR_NVS_NEW_VERSION_FOUND) {
        nvs_flash_erase();
        nvs_flash_init();
    }
    s_config_lock = xSemaphoreCreateMutex();
    if (!s_config_lock) {
        ESP_LOGE(TAG, "could not create the configuration lock");
        return;
    }
    derive_identity();
    theme_load();
    ESP_ERROR_CHECK(esp_netif_init());
    ESP_ERROR_CHECK(esp_event_loop_create_default());
    s_ev = xEventGroupCreate();

    const wifi_init_config_t ic = WIFI_INIT_CONFIG_DEFAULT();
    ESP_ERROR_CHECK(esp_wifi_init(&ic));
    ESP_ERROR_CHECK(esp_event_handler_instance_register(WIFI_EVENT, ESP_EVENT_ANY_ID,
                                                        on_wifi, NULL, NULL));
    ESP_ERROR_CHECK(esp_event_handler_instance_register(IP_EVENT, IP_EVENT_STA_GOT_IP,
                                                        on_wifi, NULL, NULL));
    ESP_ERROR_CHECK(esp_event_handler_instance_register(IP_EVENT, IP_EVENT_AP_STAIPASSIGNED,
                                                        on_wifi, NULL, NULL));

    if (!creds_load()) { start_ap(); return; }

    esp_netif_create_default_wifi_sta();
    wifi_config_t sta = {0};
    strncpy((char *)sta.sta.ssid, s_ssid, sizeof sta.sta.ssid - 1);
    strncpy((char *)sta.sta.password, s_pass, sizeof sta.sta.password - 1);
    ESP_ERROR_CHECK(esp_wifi_set_mode(WIFI_MODE_STA));
    ESP_ERROR_CHECK(esp_wifi_set_config(WIFI_IF_STA, &sta));
    s_provisioning = false;
    ESP_ERROR_CHECK(esp_wifi_start());
    serve_http();          /* help + re-provisioning stay reachable on the LAN */
    xTaskCreate(net_task, "net", 4096, NULL, 4, NULL);
}
