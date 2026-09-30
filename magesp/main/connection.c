#include "connection.h"
#include <ctype.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "cJSON.h"

static bool copy_value(char *out, size_t capacity, const char *value)
{
    if (!value || strlen(value) >= capacity) return false;
    for (const unsigned char *p = (const unsigned char *)value; *p; ++p)
        if (*p < 32 || *p == 127) return false;
    memcpy(out, value, strlen(value) + 1);
    return true;
}

bool connection_origin(const char *value, char *out, size_t capacity)
{
    if (!value || !capacity) return false;
    char normalized[128];
    if (!copy_value(normalized, sizeof normalized, value)) return false;
    size_t n = strlen(normalized);
    while (n && normalized[n - 1] == '/') normalized[--n] = 0;
    for (size_t i = 0; i < n; ++i) {
        unsigned char c = (unsigned char)normalized[i];
        if (isspace(c) || c >= 127) return false;
        normalized[i] = (char)tolower(c);
    }
    size_t prefix;
    if (!strncmp(normalized, "https://", 8)) prefix = 8;
    else if (!strncmp(normalized, "http://", 7)) prefix = 7;
    else return false;
    char *host = normalized + prefix;
    if (!*host || strpbrk(host, "/?#@\\")) return false;
    char *port = NULL;
    if (*host == '[') {
        char *end = strchr(host, ']');
        if (!end || end == host + 1) return false;
        for (char *p = host + 1; p < end; ++p)
            if (!isxdigit((unsigned char)*p) && *p != ':' && *p != '.') return false;
        if (end[1]) {
            if (end[1] != ':') return false;
            port = end + 1;
        }
    } else {
        port = strchr(host, ':');
        char *end = port ? port : normalized + n;
        if (end == host) return false;
        for (char *p = host; p < end; ++p)
            if (!isalnum((unsigned char)*p) && *p != '-' && *p != '.') return false;
    }
    if (port) {
        unsigned number = 0;
        if (!port[1]) return false;
        for (char *p = port + 1; *p; ++p) {
            if (!isdigit((unsigned char)*p)) return false;
            number = number * 10 + (unsigned)(*p - '0');
            if (number > 65535) return false;
        }
        if (!number) return false;
        if ((prefix == 8 && number == 443) || (prefix == 7 && number == 80)) *port = 0;
        else snprintf(port, sizeof normalized - (size_t)(port - normalized), ":%u", number);
    }
    return copy_value(out, capacity, normalized);
}

bool connection_valid(const connection_profile_t *p)
{
    if (p->version != 1) return false;
    #define TERMINATED(field) if (!memchr(p->field, 0, sizeof p->field)) return false
    TERMINATED(url); TERMINATED(thread); TERMINATED(cfid); TERMINATED(cfsec); TERMINATED(token);
    #undef TERMINATED
    if ((!p->cfid[0]) != (!p->cfsec[0])) return false;
    char checked[128];
    if (p->url[0]) {
        if (!connection_origin(p->url, checked, sizeof checked) || strcmp(checked, p->url)) return false;
    } else if (p->token[0] || p->cfid[0] || p->cfsec[0]) return false;
    return copy_value(checked, sizeof checked, p->thread)
        && copy_value(checked, sizeof checked, p->cfid)
        && copy_value(checked, sizeof checked, p->cfsec)
        && copy_value(checked, sizeof checked, p->token);
}

bool connection_prepare(const connection_profile_t *current,
                        const char *url, const char *thread,
                        const char *cfid, const char *cfsec, bool repair,
                        connection_profile_t *out)
{
    if (!connection_valid(current)) return false;
    connection_profile_t next = *current;
    next.version = 1;
    if (url) {
        if (!connection_origin(url, next.url, sizeof next.url)) return false;
        if (strcmp(next.url, current->url)) {
            memset(next.token, 0, sizeof next.token);
            memset(next.cfid, 0, sizeof next.cfid);
            memset(next.cfsec, 0, sizeof next.cfsec);
        }
    }
    if ((cfid == NULL) != (cfsec == NULL)) return false;
    if (cfid && (!copy_value(next.cfid, sizeof next.cfid, cfid)
                 || !copy_value(next.cfsec, sizeof next.cfsec, cfsec))) return false;
    if (thread && (!*thread || !copy_value(next.thread, sizeof next.thread, thread))) return false;
    if (repair) memset(next.token, 0, sizeof next.token);
    if (!connection_valid(&next)) return false;
    *out = next;
    return true;
}

static int hex_digit(char c)
{
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

int connection_form_get(const char *body, const char *key, char *out, size_t capacity)
{
    bool found = false;
    if (!capacity) return -1;
    out[0] = 0;
    const size_t key_len = strlen(key);
    for (const char *part = body; *part;) {
        const char *end = strchr(part, '&');
        if (!end) end = part + strlen(part);
        if ((size_t)(end - part) > key_len && !strncmp(part, key, key_len) && part[key_len] == '=') {
            if (found) return -1;
            found = true;
            size_t n = 0;
            for (const char *p = part + key_len + 1; p < end; ++p) {
                unsigned char c = (unsigned char)*p;
                if (c == '+') c = ' ';
                else if (c == '%') {
                    if (end - p < 3 || hex_digit(p[1]) < 0 || hex_digit(p[2]) < 0) return -1;
                    c = (unsigned char)(hex_digit(p[1]) * 16 + hex_digit(p[2]));
                    p += 2;
                }
                if (!c || c < 32 || c == 127 || n + 1 >= capacity) return -1;
                out[n++] = (char)c;
            }
            out[n] = 0;
        }
        part = *end ? end + 1 : end;
    }
    return found ? 1 : 0;
}

bool connection_pair_token(const char *json, size_t length, char *out, size_t capacity)
{
    cJSON *root = cJSON_ParseWithLength(json, length);
    cJSON *token = cJSON_GetObjectItemCaseSensitive(root, "token");
    bool ok = cJSON_IsString(token) && token->valuestring[0]
        && copy_value(out, capacity, token->valuestring);
    cJSON_Delete(root);
    return ok;
}

bool connection_probe_valid(int status, const char *json, size_t length, const char *device_id)
{
    if (status != 200) return false;
    cJSON *root = cJSON_ParseWithLength(json, length);
    cJSON *device = cJSON_GetObjectItemCaseSensitive(root, "device_id");
    cJSON *principal = cJSON_GetObjectItemCaseSensitive(root, "principal");
    cJSON *workspace = cJSON_GetObjectItemCaseSensitive(root, "workspace");
    bool ok = cJSON_IsString(device) && !strcmp(device->valuestring, device_id)
        && cJSON_IsString(principal) && principal->valuestring[0]
        && cJSON_IsString(workspace) && workspace->valuestring[0];
    cJSON_Delete(root);
    return ok;
}
