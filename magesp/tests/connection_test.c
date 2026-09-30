#include "connection.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

int main(void)
{
    connection_profile_t original = {
        .version = 1, .url = "https://old.example", .thread = "general",
        .cfid = "old-client", .cfsec = "old-secret", .token = "old-bearer"
    }, next;
    assert(connection_valid(&original));
    assert(connection_prepare(&original, "HTTPS://OLD.EXAMPLE:443/", NULL, NULL, NULL, false, &next));
    assert(!memcmp(&next, &original, sizeof next));
    assert(connection_prepare(&original, "https://linux.example", NULL, NULL, NULL, false, &next));
    assert(!strcmp(next.url, "https://linux.example"));
    assert(!next.token[0] && !next.cfid[0] && !next.cfsec[0]);
    assert(!strcmp(next.thread, "general"));
    assert(connection_prepare(&original, "https://linux.example", NULL, "new-client", "new-secret", false, &next));
    assert(!next.token[0] && !strcmp(next.cfid, "new-client") && !strcmp(next.cfsec, "new-secret"));
    assert(connection_prepare(&original, NULL, NULL, NULL, NULL, true, &next));
    assert(!next.token[0] && !strcmp(next.cfsec, original.cfsec));
    assert(connection_prepare(&original, NULL, NULL, "", "", false, &next));
    assert(!next.cfid[0] && !next.cfsec[0] && !strcmp(next.token, original.token));
    next = original;
    assert(!connection_prepare(&original, NULL, NULL, "half-pair", NULL, false, &next));
    assert(!connection_prepare(&original, NULL, NULL, "half-pair", "", false, &next));
    assert(!connection_prepare(&original, NULL, NULL, "id", "secret\r\nInjected: value", false, &next));
    assert(!memcmp(&next, &original, sizeof next));

    char out[128];
    assert(connection_origin("http://[::1]:080/", out, sizeof out) && !strcmp(out, "http://[::1]"));
    assert(connection_origin("https://HOST.example:03002/", out, sizeof out) && !strcmp(out, "https://host.example:3002"));
    const char *bad[] = {"", "https://", "ftp://host", "https://user@host", "https://host/path",
                        "https://host?key=value", "https://host#fragment", "https://host:65536",
                        "https://host:0", "https://host:abc", "https://host\\evil", "https://host\n"};
    for (size_t i = 0; i < sizeof bad / sizeof *bad; ++i)
        assert(!connection_prepare(&original, bad[i], NULL, NULL, NULL, false, &next));
    char too_long[160]; memset(too_long, 'x', sizeof too_long - 1); too_long[sizeof too_long - 1] = 0;
    assert(!connection_prepare(&original, NULL, too_long, NULL, NULL, false, &next));
    next = original; memset(next.token, 'x', sizeof next.token);
    assert(!connection_valid(&next));
    next = original; next.version = 2;
    assert(!connection_valid(&next));

    assert(connection_form_get("notauth=wrong&auth=right", "auth", out, sizeof out) == 1 && !strcmp(out, "right"));
    assert(connection_form_get("notauth=wrong", "auth", out, sizeof out) == 0);
    assert(connection_form_get("auth=one&auth=two", "auth", out, sizeof out) == -1);
    assert(connection_form_get("auth=ab%00cd", "auth", out, sizeof out) == -1);
    assert(connection_form_get("auth=%GG", "auth", out, sizeof out) == -1);
    assert(connection_form_get("auth=secret", "auth", out, 5) == -1);
    assert(connection_form_get("url=https%3A%2F%2Flinux.example%2F", "url", out, sizeof out) == 1);
    assert(!strcmp(out, "https://linux.example/"));

    const char *identity = "{\"device_id\":\"esp-test\",\"principal\":\"owner\",\"workspace\":\"work\"}";
    assert(connection_probe_valid(200, identity, strlen(identity), "esp-test"));
    assert(!connection_probe_valid(401, identity, strlen(identity), "esp-test"));
    assert(!connection_probe_valid(200, identity, strlen(identity), "another-device"));
    assert(!connection_probe_valid(200, "{\"status\":\"ok\"}", 15, "esp-test"));
    assert(!connection_probe_valid(200, identity, 12, "esp-test"));
    const char *missing_scope = "{\"device_id\":\"esp-test\",\"principal\":\"owner\",\"workspace\":\"\"}";
    assert(!connection_probe_valid(200, missing_scope, strlen(missing_scope), "esp-test"));
    const char *token = "{\"token\":\"fresh-bearer\"}";
    assert(connection_pair_token(token, strlen(token), out, sizeof out) && !strcmp(out, "fresh-bearer"));
    assert(!connection_pair_token("{\"token\":\"\"}", 12, out, sizeof out));
    assert(!connection_pair_token(token, strlen(token), out, 5));
    assert(!strcmp(original.token, "old-bearer"));
    puts("ESP32 connection contracts passed: origin switches, credential isolation, repair, form parsing and authenticated readiness.");
    return 0;
}
