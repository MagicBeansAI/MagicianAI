#include "realtime.h"
#include "net.h"
#include "voice.h"
#include "audio.h"

#include "esp_timer.h"

#include <stdio.h>
#include <string.h>

#include "esp_crt_bundle.h"
#include "esp_heap_caps.h"
#include "esp_http_client.h"
#include "esp_log.h"
#include "esp_websocket_client.h"
#include "freertos/FreeRTOS.h"
#include "freertos/event_groups.h"
#include "freertos/task.h"

static const char *TAG = "realtime";

#define SESSIONS_PATH "/api/magician/v2/media/sessions"
#define CONTROL_PATH  "/api/magician/v2/media/voice/%s/control"

/* The profile is not compiled in for the same reason the hostname is not: the
   device should learn what to talk to, not assert it. Overridable via /config
   once this is more than a spike. */
#define DEFAULT_PROFILE "voice_realtime_openai_backend"

/* The provider's rate, both directions. The ES8311 is duplex on one I2S and
   both halves share a clock, so a single rate is simpler here than the turn
   path's 16 kHz in / 22.05 kHz out. */
#define RT_RATE 24000
#define AUDIO_IDLE_DONE_MS 1200

#define WS_CONNECTED  BIT0
#define WS_CLOSED     BIT1

static volatile bool s_running;
static EventGroupHandle_t s_events;
static char s_session[96];

/* A live call, as distinct from the heap probe. */
static volatile rt_state_t s_state;
static volatile uint32_t   s_uplink_bytes;
static volatile uint32_t   s_uplink_drops;
static volatile uint32_t   s_downlink_bytes;
static volatile uint32_t   s_responses;
static volatile uint32_t   s_last_downlink_ms;
static volatile bool       s_stop;
static char                s_error[96];
static int64_t             s_started_us;
static esp_websocket_client_handle_t s_ws;

rt_state_t  realtime_state(void) { return s_state; }
const char *realtime_state_name(void)
{
    switch (s_state) {
    case RT_OPENING:  return "opening";
    case RT_LIVE:     return "live";
    case RT_SPEAKING: return "speaking";
    case RT_FAILED:   return "failed";
    default:          return "off";
    }
}
const char *realtime_error(void) { return s_error; }
uint32_t realtime_uplink_bytes(void)   { return s_uplink_bytes; }
uint32_t realtime_uplink_drops(void)   { return s_uplink_drops; }
uint32_t realtime_downlink_bytes(void) { return s_downlink_bytes; }
uint32_t realtime_responses(void)      { return s_responses; }
int realtime_seconds(void)
{
    if (s_started_us == 0) return 0;
    return (int)((esp_timer_get_time() - s_started_us) / 1000000);
}

static void fail_call(const char *why)
{
    if (s_error[0] == 0) snprintf(s_error, sizeof s_error, "%s", why);
    s_state = RT_FAILED;
    ESP_LOGE(TAG, "call failed: %s", s_error);
}

static bool turn_voice_busy(void)
{
    const turn_state_t state = voice_state();
    return state == TURN_SENDING || state == TURN_THINKING || state == TURN_SPEAKING;
}

/* One place to say what the heap is doing, because `free` alone has lied on
   this board before -- fragmentation fails a big allocation while free still
   looks comfortable. */
static void heap_note(const char *stage)
{
    ESP_LOGI(TAG, "heap @ %-18s free %u, largest block %u",
             stage,
             (unsigned)heap_caps_get_free_size(MALLOC_CAP_DEFAULT),
             (unsigned)heap_caps_get_largest_free_block(MALLOC_CAP_DEFAULT));
}

static void auth_headers(esp_http_client_handle_t cl)
{
    esp_http_client_set_header(cl, "Content-Type", "application/json");
    if (net_has_cf_token()) {
        esp_http_client_set_header(cl, "CF-Access-Client-Id", net_cf_id());
        esp_http_client_set_header(cl, "CF-Access-Client-Secret", net_cf_secret());
    }
    if (net_has_device_token()) {
        char bearer[112];
        snprintf(bearer, sizeof bearer, "Bearer %s", net_device_token());
        esp_http_client_set_header(cl, "X-Magician-Device-Id", net_device_id());
        esp_http_client_set_header(cl, "Authorization", bearer);
    }
}

/* Minimal string field lift. The response is small and known; a JSON parser
   would cost more heap than the thing being measured. */
static bool json_str(const char *src, const char *key, char *out, size_t cap)
{
    char pat[48];
    snprintf(pat, sizeof pat, "\"%s\"", key);
    const char *p = strstr(src, pat);
    if (p == NULL) return false;
    p = strchr(p + strlen(pat), ':');
    if (p == NULL) return false;
    while (*p == ':' || *p == ' ') p++;
    if (*p != '"') return false;
    p++;
    size_t n = 0;
    while (*p && *p != '"' && n + 1 < cap) out[n++] = *p++;
    out[n] = 0;
    return n > 0;
}

/* Step 1a: register the media session the control WS will look up.
 *
 * surface_type is `esp_terminal`, the SurfaceType variant added in stage 1 and
 * already carrying this device's turn-based traffic. audio_surface hands_free
 * describes the continuous-microphone hardware surface; session.start selects
 * the native realtime provider and its server-side turn boundary separately. */
static bool register_session_as(const char *surface)
{
    char url[256];
    snprintf(url, sizeof url, "%s" SESSIONS_PATH, net_base_url());

    static char body[512];
    snprintf(body, sizeof body,
             "{\"surface_type\":\"%s\",\"audio_surface\":\"hands_free\","
             "\"transport\":\"websocket\","
             "\"thread_id\":\"%s\",\"display_label\":\"%s\","
             /* Registered all-false by default, which would be a surface that
                cannot do the one thing it exists for. */
             "\"capabilities\":{\"realtime_voice\":true,\"mic\":true,"
             "\"provider_tts\":true}}",
             surface, net_thread(), net_device_id());

    esp_http_client_config_t cfg = {
        .url = url,
        .method = HTTP_METHOD_POST,
        .timeout_ms = 15000,
        .crt_bundle_attach = esp_crt_bundle_attach,
        .buffer_size = 1024,
        .buffer_size_tx = 1024,
    };
    esp_http_client_handle_t cl = esp_http_client_init(&cfg);
    if (cl == NULL) {
        ESP_LOGE(TAG, "cannot create the session client");
        return false;
    }
    auth_headers(cl);

    /* open/write/fetch_headers rather than perform().
     *
     * esp_http_client_perform() consumes the body internally, so a later
     * read_response() returns nothing -- which is exactly how the first run of
     * this spike reported "no session_id" against a response that plainly
     * contained one. voice.c already uses this pattern for the same reason. */
    const int blen = (int)strlen(body);
    esp_err_t err = esp_http_client_open(cl, blen);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "cannot open the session request: %s", esp_err_to_name(err));
        esp_http_client_cleanup(cl);
        return false;
    }
    if (esp_http_client_write(cl, body, blen) != blen) {
        ESP_LOGE(TAG, "short write on the session request");
        esp_http_client_cleanup(cl);
        return false;
    }
    esp_http_client_fetch_headers(cl);
    const int status = esp_http_client_get_status_code(cl);

    static char resp[900];
    const int n = esp_http_client_read_response(cl, resp, sizeof resp - 1);
    resp[n > 0 ? n : 0] = 0;
    esp_http_client_cleanup(cl);

    if (status < 200 || status >= 300) {
        ESP_LOGE(TAG, "session register as \"%s\" failed: HTTP %d -- %.180s",
                 surface, status, resp);
        return false;
    }

    if (!json_str(resp, "session_id", s_session, sizeof s_session)) {
        ESP_LOGE(TAG, "no session_id in the response: %.160s", resp);
        return false;
    }
    ESP_LOGI(TAG, "media session registered: %s", s_session);
    return true;
}

/* The deployed Magician does not know `esp_terminal` yet.
 *
 * SurfaceType::EspTerminal was added to the source in stage 1 but the running
 * binary predates it -- and nothing noticed, because the turn-based path sends
 * `source_surface` as a form STRING and never goes through this enum. Every
 * other variant registers; ours returns 400 "Invalid JSON payload".
 *
 * So try the honest label first and fall back only to keep the measurement
 * moving. The fallback disappears by itself the day the server is rebuilt,
 * which is the point: this is a note about the deployment, not a workaround
 * pretending to be a design. */
static bool register_session(void)
{
    if (register_session_as("esp_terminal")) return true;

    ESP_LOGW(TAG, "the deployed server does not know surface_type=esp_terminal; "
                  "it needs a rebuild. Falling back to tray_macos SO THE HEAP "
                  "MEASUREMENT CAN PROCEED -- this is not a fix.");
    return register_session_as("tray_macos");
}

/* Websocket text frames are length-delimited and are not promised to carry a
   trailing NUL. Keep control-event matching inside the received frame. */
static bool frame_contains(const char *frame, size_t frame_len, const char *needle)
{
    const size_t needle_len = strlen(needle);
    if (frame == NULL || needle_len == 0 || frame_len < needle_len) return false;
    for (size_t i = 0; i <= frame_len - needle_len; i++) {
        if (memcmp(frame + i, needle, needle_len) == 0) return true;
    }
    return false;
}

static void ws_event(void *arg, esp_event_base_t base, int32_t id, void *data)
{
    (void)arg; (void)base;
    esp_websocket_event_data_t *ev = (esp_websocket_event_data_t *)data;

    switch (id) {
    case WEBSOCKET_EVENT_CONNECTED:
        ESP_LOGI(TAG, "ws connected");
        xEventGroupSetBits(s_events, WS_CONNECTED);
        break;
    case WEBSOCKET_EVENT_CLOSED:
        if (!s_stop && s_error[0] == 0) {
            snprintf(s_error, sizeof s_error,
                     "websocket closed: code=%d type=%d status=%d errno=%d",
                     ev->close_status_code, ev->error_handle.error_type,
                     ev->error_handle.esp_ws_handshake_status_code,
                     ev->error_handle.esp_transport_sock_errno);
        }
        ESP_LOGW(TAG, "ws closed by peer: %s", s_error);
        xEventGroupSetBits(s_events, WS_CLOSED);
        break;
    case WEBSOCKET_EVENT_DISCONNECTED:
        /* Why, in the server's words where it gave any. A bare "disconnected"
           is the same non-answer as a silent probe. */
        ESP_LOGW(TAG, "ws disconnected (hs status %d, errno %d, len %d): %.*s",
                 ev->error_handle.esp_ws_handshake_status_code,
                 ev->error_handle.esp_transport_sock_errno,
                 ev->data_len,
                 ev->data_len > 160 ? 160 : ev->data_len,
                 ev->data_ptr ? (char *)ev->data_ptr : "");
        if (!s_stop && s_error[0] == 0) {
            snprintf(s_error, sizeof s_error,
                     "websocket disconnected: type=%d status=%d errno=%d tls=%d",
                     ev->error_handle.error_type,
                     ev->error_handle.esp_ws_handshake_status_code,
                     ev->error_handle.esp_transport_sock_errno,
                     ev->error_handle.esp_tls_last_esp_err);
        }
        xEventGroupSetBits(s_events, WS_CLOSED);
        break;
    case WEBSOCKET_EVENT_DATA:
        /* op 0x2 is binary: assistant PCM16 at 24 kHz mono, which is exactly
           what the speaker ring and drain task already take. No decode, no
           resample, no second buffer -- the reply path built for the turn loop
           is reused verbatim. */
        if (ev->op_code == 0x02 && ev->data_len > 0) {
            if (s_state == RT_LIVE) {
                /* Hand the codec over: the mic is streaming right now and the
                   speaker cannot open underneath it. */
                audio_stream_stop();
                if (audio_speaker_open(RT_RATE)) {
                    s_state = RT_SPEAKING;
                } else {
                    ESP_LOGW(TAG, "cannot open the speaker for assistant audio");
                    break;
                }
            }
            if (s_state == RT_SPEAKING) {
                const int written = audio_speaker_write(ev->data_ptr, ev->data_len);
                if (written > 0) {
                    s_downlink_bytes += (uint32_t)written;
                    s_last_downlink_ms = (uint32_t)(esp_timer_get_time() / 1000);
                }
            }
        } else if (ev->op_code == 0x01 && ev->data_len > 0) {
            /* Only the first fragment carries the JSON key; the rest are
               continuations of one large frame. Logging every one of them
               buried the session in prompt text on the first run. */
            const size_t frame_len = (size_t)ev->data_len;
            if (frame_len >= 7 &&
                memcmp((const char *)ev->data_ptr, "{\"kind\"", 7) == 0) {
                ESP_LOGI(TAG, "ws event: %.*s",
                         ev->data_len > 120 ? 120 : ev->data_len,
                         (char *)ev->data_ptr);
                if (frame_contains((const char *)ev->data_ptr, frame_len,
                                   "session.ready")) {
                    s_state = RT_LIVE;
                    ESP_LOGI(TAG, "call is LIVE");
                }
                /* Magician normalizes provider response.done into the public
                   audio.output.ended event. Accept the provider spelling too
                   for compatibility with older direct relays. At this point
                   all binary audio has already been forwarded, so the speaker
                   can yield the codec back to the microphone. */
                if ((frame_contains((const char *)ev->data_ptr, frame_len,
                                    "audio.output.ended") ||
                     frame_contains((const char *)ev->data_ptr, frame_len,
                                    "response.done")) &&
                    s_state == RT_SPEAKING) {
                    audio_speaker_close();
                    s_responses++;
                    s_last_downlink_ms = 0;
                    s_state = RT_LIVE;
                }
            }
        }
        break;
    case WEBSOCKET_EVENT_ERROR:
        if (s_stop) {
            ESP_LOGI(TAG, "ignoring websocket error during intentional stop");
            break;
        }
        if (s_error[0] == 0) {
            if (ev->data_ptr != NULL && ev->data_len > 0) {
                snprintf(s_error, sizeof s_error, "%.*s",
                         ev->data_len > (int)sizeof s_error - 1
                             ? (int)sizeof s_error - 1
                             : ev->data_len,
                         ev->data_ptr);
            } else {
                snprintf(s_error, sizeof s_error,
                         "websocket error: type=%d status=%d",
                         ev->error_handle.error_type,
                         ev->error_handle.esp_ws_handshake_status_code);
            }
        }
        ESP_LOGE(TAG, "ws error: %s", s_error);
        break;
    default:
        break;
    }
}

static void spike_task(void *arg)
{
    const int hold_secs = (int)(intptr_t)arg;

    heap_note("start");

    /* Let the transport say what is wrong in its own words. A bare "ws error"
       is the same non-answer as a silent probe, and this spike exists to
       produce a cause rather than a symptom. */
    esp_log_level_set("websocket_client", ESP_LOG_DEBUG);
    esp_log_level_set("transport_ws", ESP_LOG_DEBUG);
    esp_log_level_set("esp-tls", ESP_LOG_VERBOSE);
    esp_log_level_set("esp-tls-mbedtls", ESP_LOG_VERBOSE);

    /* Two TLS sessions have never fitted on this part, so the probe stands
       down for the whole spike exactly as it does for a turn. */
    net_set_busy(true);

    if (!register_session()) goto done;
    heap_note("session registered");

    char url[320];
    char path[160];
    snprintf(path, sizeof path, CONTROL_PATH, s_session);
    /* The base URL is https://…; the websocket client wants wss://…. */
    const char *host = net_base_url();
    const char *after = strstr(host, "://");
    snprintf(url, sizeof url, "wss://%s%s", after ? after + 3 : host, path);
    ESP_LOGI(TAG, "opening %s", url);

    /* Deliberately no Origin header. validate_origin() returns Ok when there
       is none -- "Non-browser clients don't send Origin" -- so an embedded
       client passes the CSWSH check by construction. */
    static char hdrs[512];
    int h = 0;
    if (net_has_cf_token())
        h += snprintf(hdrs + h, sizeof hdrs - h,
                      "CF-Access-Client-Id: %s\r\nCF-Access-Client-Secret: %s\r\n",
                      net_cf_id(), net_cf_secret());
    if (net_has_device_token())
        h += snprintf(hdrs + h, sizeof hdrs - h,
                      "X-Magician-Device-Id: %s\r\nAuthorization: Bearer %s\r\n",
                      net_device_id(), net_device_token());

    const esp_websocket_client_config_t cfg = {
        .uri = url,
        .headers = hdrs,
        .crt_bundle_attach = esp_crt_bundle_attach,
        /* 6144 was not enough for an mbedtls handshake to run inside the
           websocket task; the socket failed ~2 s in with a bare "ws error". */
        .task_stack = 10240,
        /* 2048 was not enough for the websocket HANDSHAKE RESPONSE headers.
           The TLS handshake succeeded and the server answered; the client then
           failed with "Header size exceeded buffer size" because this buffer
           holds the upgrade response, and a reply through Cloudflare Access
           carries a lot of it -- cf-ray, cf-cache-status, Access cookies. The
           same client against a bare origin would have fitted, which is why
           this had to be measured against the real endpoint. */
        .buffer_size = 4096,
        .network_timeout_ms = 15000,
        .reconnect_timeout_ms = 10000,
        .disable_auto_reconnect = true,   /* a spike measures, it does not retry */
    };
    esp_websocket_client_handle_t ws = esp_websocket_client_init(&cfg);
    if (ws == NULL) {
        ESP_LOGE(TAG, "cannot create the websocket client");
        goto done;
    }
    esp_websocket_register_events(ws, WEBSOCKET_EVENT_ANY, ws_event, NULL);
    heap_note("ws client created");

    /* stop()/destroy() posts CLOSED after the previous waiter has gone away.
       Never let that stale bit make this new socket look as though it failed
       before its own CONNECTED event arrives. */
    xEventGroupClearBits(s_events, WS_CONNECTED | WS_CLOSED);
    if (esp_websocket_client_start(ws) != ESP_OK) {
        ESP_LOGE(TAG, "ws start failed");
        esp_websocket_client_destroy(ws);
        goto done;
    }

    const EventBits_t bits = xEventGroupWaitBits(
        s_events, WS_CONNECTED | WS_CLOSED, pdTRUE, pdFALSE, pdMS_TO_TICKS(20000));
    if (!(bits & WS_CONNECTED)) {
        ESP_LOGE(TAG, "ws never connected -- this is the answer the spike exists for");
        esp_websocket_client_stop(ws);
        esp_websocket_client_destroy(ws);
        goto done;
    }
    heap_note("ws CONNECTED");

    /* This is MODE_REALTIME, so select the named native provider and let its
       existing server VAD commit turns. Declaring hands_free here selects the
       separate cascaded provider and makes the named realtime profile inert.
       The owner explicitly tapped to begin this call, so speech inside it is
       already addressed; requiring a wake prefix makes valid audio look like
       silence when STT clips the first word. echo_cancellation remains false
       because ESP-SR has no C6 AEC; this device enforces half duplex by
       stopping the mic when assistant PCM arrives. */
    static char start[384];
    snprintf(start, sizeof start,
             "{\"kind\":\"session.start\",\"payload\":{"
             "\"realtime_profile\":\"%s\",\"voice_mode\":\"realtime\","
             "\"turn_boundary\":\"server_vad\","
             "\"require_voice_prefix\":false,"
             "\"echo_cancellation\":false,\"thread_id\":\"%s\"}}",
             DEFAULT_PROFILE, net_thread());
    const int sent = esp_websocket_client_send_text(ws, start, (int)strlen(start),
                                                    pdMS_TO_TICKS(5000));
    ESP_LOGI(TAG, "session.start sent: %d bytes", sent);
    heap_note("session.start sent");

    for (int i = 0; i < hold_secs; i++) {
        vTaskDelay(pdMS_TO_TICKS(1000));
        if (i == hold_secs / 2) heap_note("mid-hold");
    }
    heap_note("held, closing");

    esp_websocket_client_send_text(ws, "{\"kind\":\"session.end\"}", 22,
                                   pdMS_TO_TICKS(2000));
    vTaskDelay(pdMS_TO_TICKS(300));
    esp_websocket_client_stop(ws);
    esp_websocket_client_destroy(ws);
    heap_note("ws destroyed");

done:
    net_set_busy(false);
    heap_note("finished");
    s_running = false;
    vTaskDelete(NULL);
}

/* Build the wss:// URL and the auth headers for a control-WS connection.
   Shared by the call and the heap probe so they cannot drift apart. */
static void control_url(char *url, size_t ucap, char *hdrs, size_t hcap)
{
    char path[160];
    snprintf(path, sizeof path, CONTROL_PATH, s_session);
    const char *host  = net_base_url();
    const char *after = strstr(host, "://");
    snprintf(url, ucap, "wss://%s%s", after ? after + 3 : host, path);

    /* Deliberately no Origin. validate_origin() returns Ok when there is none
       -- "Non-browser clients don't send Origin" -- so an embedded client
       passes the CSWSH check by construction. */
    int h = 0;
    if (net_has_cf_token())
        h += snprintf(hdrs + h, hcap - h,
                      "CF-Access-Client-Id: %s\r\nCF-Access-Client-Secret: %s\r\n",
                      net_cf_id(), net_cf_secret());
    if (net_has_device_token())
        h += snprintf(hdrs + h, hcap - h,
                      "X-Magician-Device-Id: %s\r\nAuthorization: Bearer %s\r\n",
                      net_device_id(), net_device_token());
}

static esp_websocket_client_handle_t open_control_ws(void)
{
    static char url[320];
    static char hdrs[512];
    control_url(url, sizeof url, hdrs, sizeof hdrs);
    ESP_LOGI(TAG, "opening %s", url);

    const esp_websocket_client_config_t cfg = {
        .uri = url,
        .headers = hdrs,
        .crt_bundle_attach = esp_crt_bundle_attach,
        /* 6144 was not enough for an mbedtls handshake inside the websocket
           task; it failed ~2 s in with a bare "ws error" and no cause. */
        .task_stack = 10240,
        .buffer_size = 4096,
        .network_timeout_ms = 15000,
        .reconnect_timeout_ms = 10000,
        .disable_auto_reconnect = true,
    };
    esp_websocket_client_handle_t ws = esp_websocket_client_init(&cfg);
    if (ws == NULL) return NULL;
    esp_websocket_register_events(ws, WEBSOCKET_EVENT_ANY, ws_event, NULL);
    if (esp_websocket_client_start(ws) != ESP_OK) {
        esp_websocket_client_destroy(ws);
        return NULL;
    }
    return ws;
}

static void send_session_start(esp_websocket_client_handle_t ws)
{
    /* MODE_REALTIME uses the named native provider. Server VAD supplies the
       automatic turn boundary. Starting the call is the user's addressing
       gesture, so turns inside it do not require a repeated wake prefix. The
       device itself remains half duplex by yielding the codec to assistant PCM
       because ESP-SR has no C6 AEC. */
    static char start[384];
    snprintf(start, sizeof start,
             "{\"kind\":\"session.start\",\"payload\":{"
             "\"realtime_profile\":\"%s\",\"voice_mode\":\"realtime\","
             "\"turn_boundary\":\"server_vad\","
             "\"require_voice_prefix\":false,"
             "\"echo_cancellation\":false,\"thread_id\":\"%s\"}}",
             DEFAULT_PROFILE, net_thread());
    esp_websocket_client_send_text(ws, start, (int)strlen(start), pdMS_TO_TICKS(5000));
}

/* Mic chunk -> binary WS frame.
 *
 * Runs on the capture task, so a failure is dropped rather than retried: a
 * realtime stream that resends old audio is worse than one with a gap. The ESP
 * websocket client treats a transport write timeout as a fatal connection
 * error, though, so allow a one-second Wi-Fi/TLS scheduling stall instead of
 * killing the whole call after the former 200 ms deadline. */
static void uplink(const void *pcm, int len)
{
    if (s_ws == NULL || s_state != RT_LIVE) return;
    if (!esp_websocket_client_is_connected(s_ws)) return;
    const int sent = esp_websocket_client_send_bin(s_ws, (const char *)pcm, len,
                                                   pdMS_TO_TICKS(1000));
    if (sent != len) s_uplink_drops++;
    if (sent > 0)    s_uplink_bytes += sent;
}

/* The microphone yields while the assistant speaks.
 *
 * Not merely policy: one ES8311 on one I2S serves both directions here, and
 * this firmware opens the codec per direction. Half duplex is what the device
 * physically IS, which is also exactly what it declared to the server with
 * echo_cancellation:false -- so the two agree instead of one lying. */
static void mic_follow_state(void)
{
    const bool want = (s_state == RT_LIVE);
    if (want && !audio_streaming())  audio_stream_start(RT_RATE, uplink);
    if (!want && audio_streaming())  audio_stream_stop();
}

static void call_task(void *arg)
{
    (void)arg;
    s_error[0] = 0;
    s_state = RT_OPENING;
    s_started_us = 0;
    s_uplink_bytes = 0;
    s_uplink_drops = 0;
    s_downlink_bytes = 0;
    s_responses = 0;
    s_last_downlink_ms = 0;
    heap_note("call: start");

    net_set_busy(true);
    audio_set_lean(true);      /* shallow DMA for the whole call */

    if (!register_session())          { fail_call("could not register a session"); goto done; }
    /* A prior clean close can leave WS_CLOSED set after its waiter returned.
       These bits describe one websocket attempt, so reset them before the
       client can publish events for this call. */
    xEventGroupClearBits(s_events, WS_CONNECTED | WS_CLOSED);
    if ((s_ws = open_control_ws()) == NULL) { fail_call("could not open the socket"); goto done; }

    const EventBits_t bits = xEventGroupWaitBits(
        s_events, WS_CONNECTED | WS_CLOSED, pdTRUE, pdFALSE, pdMS_TO_TICKS(20000));
    if (!(bits & WS_CONNECTED)) { fail_call("the socket never connected"); goto done; }
    heap_note("call: ws connected");

    send_session_start(s_ws);
    s_started_us = esp_timer_get_time();

    /* STEP 2, measured in place rather than in a throwaway build: what does the
       audio hardware cost ON TOP of a live socket? The I2S DMA is the largest
       single allocation this firmware makes, and the socket has already taken
       ~54 KB. If these two do not fit together, nothing above this line
       matters. */
    uint32_t last_report = 0;

    while (!s_stop) {
        vTaskDelay(pdMS_TO_TICKS(100));
        if (!esp_websocket_client_is_connected(s_ws) && s_state != RT_FAILED) {
            fail_call("the server closed the call");
            break;
        }
        mic_follow_state();

        /* Some provider adapters account for completion without forwarding an
           audio.output.ended envelope. Binary PCM is ordered, so a bounded
           tail gap is also authoritative completion and prevents the
           half-duplex codec from remaining in speaker mode indefinitely. */
        if (s_state == RT_SPEAKING && s_last_downlink_ms != 0) {
            const uint32_t now_ms = (uint32_t)(esp_timer_get_time() / 1000);
            if ((uint32_t)(now_ms - s_last_downlink_ms) >= AUDIO_IDLE_DONE_MS) {
                audio_speaker_close();
                s_responses++;
                s_last_downlink_ms = 0;
                s_state = RT_LIVE;
                ESP_LOGI(TAG, "assistant audio completed after the tail gap");
            }
        }

        /* Say what is actually leaving the device. An uplink that is silently
           sending nothing looks exactly like a provider that is not answering,
           and this project has lost hours to that pair before. */
        const uint32_t now = (uint32_t)(esp_timer_get_time() / 1000000);
        if (now != last_report && now % 5 == 0) {
            last_report = now;
            ESP_LOGI(TAG, "uplink %u KB sent, %u drops; heap free %u",
                     (unsigned)(s_uplink_bytes / 1024), (unsigned)s_uplink_drops,
                     (unsigned)heap_caps_get_free_size(MALLOC_CAP_DEFAULT));
        }
    }

done:
    if (s_ws != NULL) {
        if (esp_websocket_client_is_connected(s_ws))
            esp_websocket_client_send_text(s_ws, "{\"kind\":\"session.end\"}", 22,
                                           pdMS_TO_TICKS(2000));
        vTaskDelay(pdMS_TO_TICKS(200));
        esp_websocket_client_stop(s_ws);
        esp_websocket_client_destroy(s_ws);
        s_ws = NULL;
    }
    audio_stream_stop();            /* no-op unless the mic was streaming */
    audio_speaker_close();          /* no-op unless a reply was mid-flight */
    audio_set_lean(false);          /* the turn path gets its deep buffers back */
    net_set_busy(false);
    if (s_state != RT_FAILED) s_state = RT_OFF;
    s_started_us = 0;
    s_running = false;
    heap_note("call: finished");
    vTaskDelete(NULL);
}

bool realtime_start(void)
{
    if (s_running) return true;
    if (turn_voice_busy() || audio_capturing()) {
        ESP_LOGW(TAG, "voice is busy -- refusing to start a call");
        snprintf(s_error, sizeof s_error, "voice is busy");
        s_state = RT_FAILED;
        return false;
    }
    if (!net_configured() || net_state() != NET_ONLINE) {
        snprintf(s_error, sizeof s_error, "not online");
        s_state = RT_FAILED;
        return false;
    }
    if (!net_has_device_token()) {
        snprintf(s_error, sizeof s_error, "device is not paired");
        s_state = RT_FAILED;
        return false;
    }
    if (s_events == NULL && (s_events = xEventGroupCreate()) == NULL) return false;

    s_stop = false;
    s_running = true;
    if (xTaskCreate(call_task, "rt-call", 5120, NULL, 4, NULL) != pdPASS) {
        ESP_LOGE(TAG, "cannot start the call task");
        s_running = false;
        return false;
    }
    return true;
}

void realtime_stop(void)
{
    if (!s_running) { s_state = RT_OFF; return; }
    ESP_LOGI(TAG, "call: stopping at %d s", realtime_seconds());
    s_stop = true;
}

bool realtime_spike(int hold_secs)
{
    if (s_running) {
        ESP_LOGW(TAG, "spike already running");
        return false;
    }
    /* A completed or failed turn remains visible as TURN_DONE/TURN_ERROR and
       is no longer active. Refuse only actual turn work plus a live capture;
       requiring TURN_IDLE made realtime unavailable after the first PTT turn
       until reboot. */
    if (turn_voice_busy() || audio_capturing()) {
        ESP_LOGW(TAG, "voice is busy (turn=%d, capturing=%d) -- refusing; "
                      "two TLS sessions have never fitted on this part",
                 (int)voice_state(), (int)audio_capturing());
        return false;
    }
    if (!net_configured() || net_state() != NET_ONLINE) {
        ESP_LOGW(TAG, "not online; nothing to connect to");
        return false;
    }
    if (!net_has_device_token()) {
        ESP_LOGW(TAG, "device is not paired; refusing unauthenticated spike");
        return false;
    }
    if (s_events == NULL) {
        s_events = xEventGroupCreate();
        if (s_events == NULL) return false;
    }
    if (hold_secs < 1)  hold_secs = 1;
    if (hold_secs > 120) hold_secs = 120;

    s_running = true;
    if (xTaskCreate(spike_task, "rt-spike", 5120, (void *)(intptr_t)hold_secs, 4,
                    NULL) != pdPASS) {
        ESP_LOGE(TAG, "cannot start the spike task");
        s_running = false;
        return false;
    }
    return true;
}
