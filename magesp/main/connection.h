#ifndef MAGESP_CONNECTION_H
#define MAGESP_CONNECTION_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/* One NVS blob keeps the destination and its credentials together. */
typedef struct {
    uint32_t version;
    char url[128];
    char thread[48];
    char cfid[80];
    char cfsec[96];
    char token[96];
} connection_profile_t;

bool connection_origin(const char *value, char *out, size_t capacity);
bool connection_valid(const connection_profile_t *profile);
/* NULL means absent; an explicit empty Access pair removes that pair.
 * A different origin always drops the old bearer and outer credentials. */
bool connection_prepare(const connection_profile_t *current,
                        const char *url, const char *thread,
                        const char *cfid, const char *cfsec, bool repair,
                        connection_profile_t *out);
/* 1 found, 0 absent, -1 malformed, duplicate or too long. */
int connection_form_get(const char *body, const char *key, char *out, size_t capacity);
bool connection_pair_token(const char *json, size_t length, char *out, size_t capacity);
bool connection_probe_valid(int status, const char *json, size_t length,
                            const char *device_id);

#endif
