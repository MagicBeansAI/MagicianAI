/* Conversations: the thread and chat this terminal talks in. See conv.h. */
#include "conv.h"

#include <stdio.h>
#include <string.h>

#include "cJSON.h"
#include "esp_crt_bundle.h"
#include "esp_http_client.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "nvs.h"

#include "net.h"
#include "voice.h"

static const char *TAG = "conv";

#define NVS_NS     "magesp"
#define K_THR      "cv_thr"
#define K_THR_NAME "cv_thr_n"
#define K_SES      "cv_ses"
#define K_SES_NAME "cv_ses_n"
#define K_PICKED   "cv_pick"
#define API        "/api/magician/v2"

static SemaphoreHandle_t s_lock;
static conv_view_t       s_view;   /* guarded by s_lock */

static void copy(char *dst, size_t cap, const char *src)
{
    snprintf(dst, cap, "%s", src ? src : "");
}

/* The panel's fonts are ASCII. Server names carry "·", "…" and dashes, which
   would draw as boxes; map the common ones and drop the rest. */
static void copy_ascii(char *dst, size_t cap, const char *src)
{
    size_t n = 0;
    const unsigned char *p = (const unsigned char *)(src ? src : "");
    while (*p && n + 4 < cap) {
        if (*p < 0x80) { dst[n++] = (char)*p++; continue; }
        if (p[0] == 0xC2 && p[1] == 0xB7) { dst[n++] = '-'; p += 2; continue; }          /* · */
        if (p[0] == 0xE2 && p[1] == 0x80 && p[2] == 0xA6) {                              /* … */
            dst[n++] = '.'; dst[n++] = '.'; dst[n++] = '.'; p += 3; continue;
        }
        if (p[0] == 0xE2 && p[1] == 0x80 && (p[2] == 0x93 || p[2] == 0x94)) { dst[n++] = '-'; p += 3; continue; }
        if (p[0] == 0xE2 && p[1] == 0x80 && (p[2] == 0x98 || p[2] == 0x99)) { dst[n++] = '\''; p += 3; continue; }
        /* Any other multi-byte character: skip all of it. */
        p++;
        while ((*p & 0xC0) == 0x80) p++;
        dst[n++] = '?';
    }
    dst[n] = 0;
}

static void changed(void) { s_view.version++; }

static void lock(void)   { if (s_lock) xSemaphoreTake(s_lock, portMAX_DELAY); }
static void unlock(void) { if (s_lock) xSemaphoreGive(s_lock); }

static void persist(void)
{
    nvs_handle_t h;
    if (nvs_open(NVS_NS, NVS_READWRITE, &h) != ESP_OK) return;
    nvs_set_str(h, K_THR, s_view.thread_id);
    nvs_set_str(h, K_THR_NAME, s_view.thread_name);
    nvs_set_str(h, K_SES, s_view.session_id);
    nvs_set_str(h, K_SES_NAME, s_view.session_title);
    nvs_set_u8(h, K_PICKED, s_view.picked ? 1 : 0);
    nvs_commit(h);
    nvs_close(h);
}

void conv_init(void)
{
    s_lock = xSemaphoreCreateMutex();
    memset(&s_view, 0, sizeof s_view);
    nvs_handle_t h;
    if (nvs_open(NVS_NS, NVS_READONLY, &h) == ESP_OK) {
        size_t n;
        n = sizeof s_view.thread_id;     nvs_get_str(h, K_THR, s_view.thread_id, &n);
        n = sizeof s_view.thread_name;   nvs_get_str(h, K_THR_NAME, s_view.thread_name, &n);
        n = sizeof s_view.session_id;    nvs_get_str(h, K_SES, s_view.session_id, &n);
        n = sizeof s_view.session_title; nvs_get_str(h, K_SES_NAME, s_view.session_title, &n);
        uint8_t picked = 0;
        nvs_get_u8(h, K_PICKED, &picked);
        s_view.picked = picked && s_view.thread_id[0];
        nvs_close(h);
    }
    ESP_LOGI(TAG, "talking in %s%s", s_view.picked ? s_view.thread_id : "the device thread",
             s_view.session_id[0] ? " (chosen chat)" : "");
}

unsigned conv_version(void)
{
    lock();
    const unsigned v = s_view.version;
    unlock();
    return v;
}

void conv_snapshot(conv_view_t *out)
{
    lock();
    *out = s_view;
    unlock();
}

const char *conv_turn_thread(bool *picked)
{
    /* Read on the turn worker, which is also the only writer of the lists;
       selection writes come from the UI under the lock. */
    static char thread[CONV_ID_LEN];
    lock();
    *picked = s_view.picked;
    copy(thread, sizeof thread, s_view.picked ? s_view.thread_id : net_thread());
    unlock();
    return thread;
}

const char *conv_turn_session(void)
{
    static char session[CONV_ID_LEN];
    lock();
    copy(session, sizeof session, s_view.picked ? s_view.session_id : "");
    unlock();
    return session;
}

/* ---- network, on the turn worker ------------------------------------- */

static int request(const char *method, const char *path, char *out, size_t cap)
{
    char url[256];
    snprintf(url, sizeof url, "%s" API "%s", net_base_url(), path);
    esp_http_client_config_t c = {
        .url = url,
        .timeout_ms = 15000,
        .crt_bundle_attach = esp_crt_bundle_attach,
        .disable_auto_redirect = true,
        .method = strcmp(method, "POST") == 0 ? HTTP_METHOD_POST : HTTP_METHOD_GET,
    };
    esp_http_client_handle_t cl = esp_http_client_init(&c);
    if (!cl) return -1;
    if (net_has_cf_token()) {
        esp_http_client_set_header(cl, "CF-Access-Client-Id", net_cf_id());
        esp_http_client_set_header(cl, "CF-Access-Client-Secret", net_cf_secret());
    }
    char bearer[112];
    snprintf(bearer, sizeof bearer, "Bearer %s", net_device_token());
    esp_http_client_set_header(cl, "Authorization", bearer);
    esp_http_client_set_header(cl, "X-Magician-Device-Id", net_device_id());
    esp_http_client_set_header(cl, "X-Magician-App-Version", net_app_version());
    esp_http_client_set_header(cl, "Accept", "application/json");
    int status = -1;
    if (esp_http_client_open(cl, 0) == ESP_OK) {
        esp_http_client_fetch_headers(cl);
        status = esp_http_client_get_status_code(cl);
        const int n = esp_http_client_read_response(cl, out, (int)cap - 1);
        out[n > 0 ? n : 0] = 0;
    }
    esp_http_client_cleanup(cl);
    return status;
}

static void fail(const char *why)
{
    lock();
    s_view.state = CONV_FAILED;
    copy(s_view.error, sizeof s_view.error, why);
    changed();
    unlock();
    ESP_LOGW(TAG, "%s", why);
}

static char s_body[4096];   /* worker-only scratch for the JSON answers */

static void fill(conv_item_t *items, int *count, int max, const cJSON *array,
                 const char *id_key, const char *name_key)
{
    int n = 0;
    const cJSON *it;
    cJSON_ArrayForEach(it, array) {
        if (n >= max) break;
        const char *id = cJSON_GetStringValue(cJSON_GetObjectItem(it, id_key));
        const char *name = cJSON_GetStringValue(cJSON_GetObjectItem(it, name_key));
        if (!id || !id[0]) continue;
        copy(items[n].id, sizeof items[n].id, id);
        copy_ascii(items[n].name, sizeof items[n].name, name && name[0] ? name : id);
        n++;
    }
    *count = n;
}

static void do_refresh(void)
{
    /* A thread picked while a request was out names other chats than the
       ones that request returns: drop that answer and ask again. */
    for (int attempt = 0; attempt < 3; attempt++) {
        char path[160];
        lock();
        const bool picked = s_view.picked;
        char thread[CONV_ID_LEN];
        copy(thread, sizeof thread, s_view.thread_id);
        unlock();
        if (picked && thread[0]) snprintf(path, sizeof path, "/devices/me/conversations?thread=%s", thread);
        else                     snprintf(path, sizeof path, "/devices/me/conversations");

        const int status = request("GET", path, s_body, sizeof s_body);
        if (status != 200) {
            char why[64];
            snprintf(why, sizeof why, status < 0 ? "magician unreachable" : "lists refused (HTTP %d)", status);
            fail(why);
            return;
        }
        cJSON *root = cJSON_Parse(s_body);
        if (!root) { fail("lists unreadable"); return; }
        lock();
        if (s_view.picked && (!picked || strcmp(s_view.thread_id, thread) != 0)) {
            unlock();
            cJSON_Delete(root);
            continue;
        }
        fill(s_view.threads, &s_view.n_threads, CONV_MAX_THREADS,
             cJSON_GetObjectItem(root, "threads"), "id", "name");
        fill(s_view.sessions, &s_view.n_sessions, CONV_MAX_SESSIONS,
             cJSON_GetObjectItem(root, "sessions"), "id", "title");
        if (!s_view.picked) {
            /* Not chosen yet: show the device's own thread as the current one. */
            const char *dt = cJSON_GetStringValue(cJSON_GetObjectItem(root, "device_thread"));
            copy(s_view.thread_id, sizeof s_view.thread_id, dt);
            s_view.thread_name[0] = 0;
        }
        for (int i = 0; i < s_view.n_threads; i++)
            if (strcmp(s_view.threads[i].id, s_view.thread_id) == 0)
                copy(s_view.thread_name, sizeof s_view.thread_name, s_view.threads[i].name);
        s_view.state = CONV_READY;
        s_view.error[0] = 0;
        changed();
        unlock();
        cJSON_Delete(root);
        return;
    }
    fail("lists kept changing; open the page again");
}

static void do_new_thread(void)
{
    const int status = request("POST", "/devices/me/threads", s_body, sizeof s_body);
    cJSON *root = status == 201 || status == 200 ? cJSON_Parse(s_body) : NULL;
    const char *id = root ? cJSON_GetStringValue(cJSON_GetObjectItem(root, "id")) : NULL;
    if (!id || !id[0]) {
        char why[64];
        snprintf(why, sizeof why, status < 0 ? "magician unreachable" : "new thread refused (HTTP %d)", status);
        cJSON_Delete(root);
        fail(why);
        return;
    }
    lock();
    s_view.picked = true;
    copy(s_view.thread_id, sizeof s_view.thread_id, id);
    copy_ascii(s_view.thread_name, sizeof s_view.thread_name,
               cJSON_GetStringValue(cJSON_GetObjectItem(root, "name")));
    s_view.session_id[0] = s_view.session_title[0] = 0;
    persist();
    changed();
    unlock();
    ESP_LOGI(TAG, "new thread %s", id);
    cJSON_Delete(root);
    do_refresh();
}

static bool start(void (*job)(void))
{
    if (!net_configured() || !net_has_device_token()) { fail("device is not paired"); return false; }
    lock();
    const conv_state_t previous = s_view.state;
    s_view.state = CONV_LOADING;
    changed();
    const unsigned version = s_view.version;
    unlock();
    /* Giving the semaphore can immediately switch to the higher-priority
       worker. Publish loading first, or a completed job is overwritten. */
    if (!voice_run_job(job)) {
        lock();
        if (s_view.version == version) {
            s_view.state = previous;
            changed();
        }
        unlock();
        return false;
    }
    return true;
}

bool conv_refresh(void)    { return start(do_refresh); }
bool conv_new_thread(void) { return start(do_new_thread); }

void conv_select_thread(int index)
{
    lock();
    if (index < 0 || index >= s_view.n_threads) { unlock(); return; }
    s_view.picked = true;
    copy(s_view.thread_id, sizeof s_view.thread_id, s_view.threads[index].id);
    copy(s_view.thread_name, sizeof s_view.thread_name, s_view.threads[index].name);
    s_view.session_id[0] = s_view.session_title[0] = 0;
    s_view.n_sessions = 0;
    persist();
    changed();
    unlock();
    conv_refresh();
}

void conv_select_session(int index)
{
    lock();
    if (index >= s_view.n_sessions) { unlock(); return; }
    if (index < 0) {
        s_view.session_id[0] = s_view.session_title[0] = 0;
    } else {
        copy(s_view.session_id, sizeof s_view.session_id, s_view.sessions[index].id);
        copy(s_view.session_title, sizeof s_view.session_title, s_view.sessions[index].name);
    }
    if (s_view.thread_id[0]) s_view.picked = true;
    persist();
    changed();
    unlock();
}
